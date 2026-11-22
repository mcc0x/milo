//! Fills in details without making the first frame depend on every new file.

use std::collections::HashMap;
use std::fs;
use std::io::Read as _;
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::fs_ops;
use crate::git::{FileStamp, Kind, PendingFile};

use super::Outline;

const FILE_LIMIT: u64 = 256 << 10;
const BYTES_PER_FRAME: u64 = 1 << 20;
const FILES_PER_FRAME: usize = 24;
const STAMPS_PER_FRAME: usize = 64;
const SHOWN_STAMPS_PER_CHECK: usize = 96;
const OTHER_STAMPS_PER_CHECK: usize = 48;
const NEXT_BATCH: Duration = Duration::from_millis(40);

struct Sweep {
    remaining: usize,
    due: Instant,
}

#[derive(Default)]
pub(super) struct Untracked {
    files: Range<usize>,
    scan: usize,
    sweep: Option<Sweep>,
    reported: bool,
    /// New paths awaiting a timestamp for recency and edit detection.
    unstamped: Vec<usize>,
    /// Pending indices for files actually painted, not all unfolded rows.
    shown: Vec<usize>,
    count_more: bool,
}

impl Untracked {
    /// Retain measured details by path without asking the filesystem again.
    pub(super) fn retain_details(&mut self, old: &[PendingFile], files: &mut [PendingFile]) {
        let start = files.partition_point(|f| f.kind != Kind::Untracked && !f.unpushed);
        let end = start + files[start..].partition_point(|f| f.kind == Kind::Untracked);
        let old = &old[self.files.clone()];
        let cached: HashMap<_, _> =
            old.iter().filter(|f| f.stamp.is_some() || !f.lines_pending).map(|f| (f.path.as_str(), f)).collect();
        self.files = start..end;
        self.shown.clear();
        self.unstamped.clear();
        self.scan %= self.files.len().max(1);
        let mut same_paths = self.sweep.is_some() && old.len() == self.files.len();
        for i in self.files.clone().rev() {
            let file = &mut files[i];
            if same_paths {
                same_paths = old[i - start].path == file.path;
            }
            if let Some(before) = cached.get(file.path.as_str()) {
                (file.add, file.lines_pending, file.stamp) = (before.add, before.lines_pending, before.stamp);
            }
            if file.stamp.is_none() {
                self.unstamped.push(i);
            }
        }
        if self.files.is_empty() {
            self.sweep = None;
        } else if !same_paths && let Some(sweep) = &mut self.sweep {
            // The cursor is relative to the untracked paths, not the whole pending list.
            // If their order changed, some paths may have moved behind the cursor.
            sweep.remaining = self.files.len();
        }
    }

    pub(super) fn start_frame(&mut self) {
        self.shown.clear();
        self.count_more = false;
    }

    pub(super) fn show(&mut self, i: usize) {
        self.shown.push(i);
    }

    pub(super) fn next_frame(&self) -> Option<Duration> {
        (self.count_more || !self.unstamped.is_empty()).then_some(NEXT_BATCH)
    }

    pub(super) fn request_sweep(&mut self) {
        self.reported = true;
    }

    pub(super) fn next_sweep(&self, now: Instant) -> Option<Duration> {
        self.sweep.as_ref().map(|sweep| sweep.due.saturating_duration_since(now))
    }

    fn start_sweep(&mut self, now: Instant) {
        if !std::mem::take(&mut self.reported) || self.files.is_empty() {
            return;
        }
        // A new edit may be behind the cursor. Extend coverage without
        // restarting the cursor, so repeated notifications cannot starve any path.
        self.sweep.get_or_insert(Sweep { remaining: 0, due: now }).remaining = self.files.len();
    }

    fn scan_batch(&mut self, now: Instant, sample: bool) -> usize {
        let Some(sweep) = &mut self.sweep else {
            return if sample { self.files.len().min(OTHER_STAMPS_PER_CHECK) } else { 0 };
        };
        if now < sweep.due {
            return 0;
        }
        let batch = sweep.remaining.min(OTHER_STAMPS_PER_CHECK);
        sweep.remaining -= batch;
        if sweep.remaining == 0 {
            self.sweep = None;
        } else {
            sweep.due = now + NEXT_BATCH;
        }
        batch
    }
}

impl Outline {
    pub(super) fn count_shown_untracked(&mut self, rows: Range<usize>, wide: bool) {
        if self.untracked.files.is_empty() {
            return;
        }
        let (mut attempts, mut bytes) = (FILES_PER_FRAME, BYTES_PER_FRAME);
        for row in rows {
            let Some(i) = self.tree.row(row).and_then(|node| node.untracked_index()) else { continue };
            self.untracked.show(i);
            if wide && !self.untracked.count_more && self.pending[i].lines_pending {
                self.untracked.count_more = !self.count_untracked(i, &mut attempts, &mut bytes);
            }
        }
    }

    fn count_untracked(&mut self, i: usize, attempts: &mut usize, bytes: &mut u64) -> bool {
        if *attempts == 0 {
            return false;
        }
        *attempts -= 1;
        let file = &mut self.pending[i];
        let path = self.repo.root.join(&file.path);
        let meta = fs::symlink_metadata(&path).ok();
        if file.stamp.is_none() {
            file.stamp = meta.as_ref().and_then(FileStamp::of);
            self.recent_dirty |= file.stamp.is_some();
        }
        let lines = match meta {
            Some(m) if m.is_symlink() => 1,
            Some(m) if m.is_file() && m.len() <= FILE_LIMIT => {
                let cost = m.len() + 1;
                if *bytes < cost {
                    return false;
                }
                // Reserve one extra byte so growth during the read cannot go over budget.
                *bytes -= cost;
                count_lines(&path, cost).unwrap_or(0)
            }
            _ => 0,
        };
        let before = (file.add, file.lines_pending);
        (file.add, file.lines_pending) = (lines, false);
        self.update_untracked_size(i, before);
        true
    }

    /// Discover the remaining timestamps in small batches; don't read offscreen files.
    pub(super) fn discover_untracked(&mut self) {
        for _ in 0..STAMPS_PER_FRAME {
            let Some(i) = self.untracked.unstamped.pop() else { break };
            let file = &mut self.pending[i];
            if file.stamp.is_none() {
                file.stamp = FileStamp::at(&self.repo.root.join(&file.path));
                self.recent_dirty |= file.stamp.is_some();
            }
        }
    }

    /// Git reports path membership, not edits to untracked file contents.
    /// Check the viewport promptly, then scan the rest separately from Git.
    pub(super) fn check_untracked(&mut self, now: Instant) {
        self.untracked.start_sweep(now);
        self.scan_untracked(now, true);
    }

    pub(super) fn scan_untracked(&mut self, now: Instant, prioritize_shown: bool) {
        let len = self.untracked.files.len();
        let batch = self.untracked.scan_batch(now, prioritize_shown);
        let shown = if prioritize_shown { self.untracked.shown.len().min(SHOWN_STAMPS_PER_CHECK) } else { 0 };
        let mut newest = None;
        for pos in 0..shown {
            self.review_untracked(self.untracked.shown[pos], now, &mut newest);
        }
        for _ in 0..batch {
            let i = self.untracked.files.start + self.untracked.scan;
            let next = self.untracked.scan + 1;
            self.untracked.scan = if next == len { 0 } else { next };
            if !self.untracked.shown[..shown].contains(&i) {
                self.review_untracked(i, now, &mut newest);
            }
        }
        if let Some(i) = newest
            && self.follows_activity(now)
        {
            let path = self.pending[i].path.clone();
            self.follow(&path);
        }
    }

    fn review_untracked(&mut self, i: usize, now: Instant, newest: &mut Option<usize>) {
        let file = &mut self.pending[i];
        let stamp = FileStamp::at(&self.repo.root.join(&file.path));
        if stamp == file.stamp {
            return;
        }
        let changed = std::mem::replace(&mut file.stamp, stamp).is_some() && stamp.is_some();
        self.recent_dirty = true;
        if changed {
            self.attention.mark(&file.path, now);
            let written = stamp.map(|s| s.written);
            if newest.is_none_or(|prev| self.pending[prev].stamp.map(|s| s.written) <= written) {
                *newest = Some(i);
            }
        }
        let file = &mut self.pending[i];
        if !file.lines_pending {
            let before = (file.add, false);
            (file.add, file.lines_pending) = (0, true);
            self.update_untracked_size(i, before);
        }
    }

    fn update_untracked_size(&mut self, i: usize, before: (u32, bool)) {
        self.tree.update_untracked(i, &self.pending[i], before, |node| {
            if let Some(columns) = &mut self.change_columns {
                columns.include_size(node);
            }
        });
    }
}

/// Counts a regular file without allocating its contents or following a link.
fn count_lines(path: &Path, limit: u64) -> Option<u32> {
    let file = fs_ops::open_read(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() >= limit {
        return None;
    }
    let mut file = file.take(limit);
    let mut chunk = [0; 8192];
    let mut lines = 0;
    let mut unfinished = false;
    loop {
        match file.read(&mut chunk) {
            Ok(0) => return Some(lines + u32::from(unfinished)),
            Ok(n) => {
                if file.limit() == 0 {
                    return None;
                }
                for &byte in &chunk[..n] {
                    match byte {
                        0 => return None,
                        b'\n' => lines += 1,
                        _ => {}
                    }
                }
                unfinished = chunk[n - 1] != b'\n';
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use crate::git::Repo;
    use crate::tree::{Node, Tree};

    #[test]
    fn editing_an_untracked_path_also_listed_as_deleted_matches_rebuild() {
        const OVERLAP: &str = "src/overlap.txt";
        const ORDINARY: &str = "src/ordinary.txt";
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-m", "base"]);
        f.write(OVERLAP, "one\ntwo\n");
        f.commit("track overlap");
        f.git(&["switch", "-c", "work", "HEAD^"]);
        for path in [OVERLAP, ORDINARY] {
            f.write(path, "one\ntwo\n");
        }

        let mut outline = Outline::new(Repo::open(&f.root).unwrap(), false);
        let (mut attempts, mut bytes) = (FILES_PER_FRAME, BYTES_PER_FRAME);
        for i in outline.untracked.files.clone() {
            assert!(outline.count_untracked(i, &mut attempts, &mut bytes));
        }

        // Move HEAD without updating the index: Git now reports OVERLAP twice.
        f.git(&["update-ref", "HEAD", "refs/heads/main"]);
        outline.load().unwrap();
        let deleted = &outline.pending[0];
        assert_eq!((deleted.path.as_str(), deleted.kind), (OVERLAP, Kind::Deleted));
        assert_eq!(outline.untracked.files.len(), 2);
        for file in &outline.pending[outline.untracked.files.clone()] {
            assert_eq!((file.add, file.lines_pending), (2, false));
            f.write(&file.path, "one\ntwo\nthree\n");
        }
        outline.check_untracked(Instant::now());
        assert!(outline.pending[outline.untracked.files.clone()].iter().all(|f| f.lines_pending));

        let mut rebuilt = Tree::default();
        rebuilt.rebuild(outline.repo.listed(), &outline.pending);
        rebuilt.layout(|_| true, false);
        let counts = |node: &Node| (node.kind, node.add, node.del, node.uncounted);
        assert_eq!(counts(outline.tree.node(0)), counts(rebuilt.node(0)));
        for path in ["src/", OVERLAP, ORDINARY] {
            let actual = outline.tree.row(outline.tree.row_of(path).unwrap()).unwrap();
            let expected = rebuilt.row(rebuilt.row_of(path).unwrap()).unwrap();
            assert_eq!(counts(actual), counts(expected), "{path}");
        }
    }
}
