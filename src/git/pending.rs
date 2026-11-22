//! Caches the line counts of the tracked changes in a status snapshot.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::{FileStamp, Kind, PATHS_LIMIT, PendingFile, Repo, parse_files};

/// Worktree inputs that can be stamped without opening a file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum DiffStamp {
    Missing,
    File { stamp: FileStamp, device: u64, inode: u64, changed: (i64, i64) },
    Uncertain(Option<FileStamp>),
}

impl DiffStamp {
    pub(super) fn at(path: &Path) -> Self {
        match path.symlink_metadata() {
            Ok(meta) => match FileStamp::of(&meta) {
                Some(stamp) if meta.is_file() || meta.is_symlink() => Self::File {
                    stamp,
                    device: meta.dev(),
                    inode: meta.ino(),
                    changed: (meta.ctime(), meta.ctime_nsec()),
                },
                // A submodule can change commits without changing the directory stamp.
                stamp => Self::Uncertain(stamp),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::Missing,
            Err(_) => Self::Uncertain(None),
        }
    }

    fn file(self) -> Option<FileStamp> {
        match self {
            Self::Missing => None,
            Self::File { stamp, .. } => Some(stamp),
            Self::Uncertain(stamp) => stamp,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct TrackedDiff {
    /// In status path order, including both ends of a rename.
    stamps: Vec<DiffStamp>,
    files: Vec<PendingFile>,
    index: Option<DiffStamp>,
}

/// Compare the porcelain records directly: names alone miss staging,
/// conflicts' stage IDs, and a rename's separate source field.
pub(super) fn tracked_records(raw: &[u8]) -> impl Iterator<Item = (&[u8], Option<&[u8]>)> {
    let mut records = raw.split(|&b| b == 0);
    std::iter::from_fn(move || {
        loop {
            let record = records.next()?;
            match record {
                [b'1' | b'u', b' ', ..] => return Some((record, None)),
                [b'2', b' ', ..] => return Some((record, records.next())),
                [b'#', b' ', ..] => {}
                // Untracked and ignored entries follow all tracked ones.
                _ => return None,
            }
        }
    })
}

impl Repo {
    /// A manual reload also checks settings outside status, such as attributes.
    pub(crate) fn invalidate_tracked_diff(&mut self) {
        self.pending_tracked = None;
    }

    pub(super) fn tracked_diff_moved(&self) -> bool {
        self.pending_tracked.as_ref().is_some_and(|known| !self.diff_stamps().eq(known.stamps.iter().copied()))
    }

    pub(super) fn tracked_uncommitted(&mut self) -> Option<Vec<PendingFile>> {
        let paths = &self.status.tracked_uncommitted;
        if paths.is_empty() {
            return Some(Vec::new());
        }
        // Keep stamps taken before Git runs so a later edit cannot be cached
        // under its newer stamp. A failed diff must drop the old counts.
        // `read` has already discarded the cache if tracked status or the
        // comparison base changed.
        let mut known = self.pending_tracked.take().unwrap_or_default();
        let same_paths = known.stamps.len() == paths.len();
        let mut cacheable = true;
        let mut changed = Vec::new();
        known.stamps.resize(paths.len(), DiffStamp::Missing);
        for (at, (stamp, previous)) in self.diff_stamps().zip(&mut known.stamps).enumerate() {
            if same_paths && stamp != *previous {
                changed.push(at);
            }
            cacheable &= !matches!(stamp, DiffStamp::Uncertain(_));
            *previous = stamp;
        }
        if !same_paths || !changed.is_empty() {
            let index = self.diff_index();
            let named = paths.len() <= PATHS_LIMIT;
            // Use the tip from status, not a possibly newer HEAD. The large
            // full diff uses the tip to exclude files that are only unpushed.
            let ends = self.ends();
            let tip = if ends.has_commit() { ends.tip.as_str() } else { self.empty_tree()? };
            let against = if named { ends.base.as_deref().unwrap_or(tip) } else { tip };
            if !(cacheable
                && same_paths
                && changed.len() < paths.len()
                && changed.len() <= PATHS_LIMIT
                && index.is_some()
                && index == known.index
                && self.recount_tracked(&mut known, &changed, against)?)
            {
                known.files = self.diff_tracked(&known.stamps, against, named)?;
            }
            known.index = index;
        }
        if cacheable {
            let mut files = Vec::with_capacity(known.files.len() + self.status.untracked.len());
            files.extend_from_slice(&known.files);
            self.pending_tracked = Some(known);
            Some(files)
        } else {
            Some(known.files)
        }
    }

    fn diff_stamps(&self) -> impl Iterator<Item = DiffStamp> + '_ {
        let mut absolute = PathBuf::new();
        self.status.tracked_uncommitted.iter().map(move |path| {
            absolute.clear();
            absolute.push(&self.root);
            absolute.push(path);
            DiffStamp::at(&absolute)
        })
    }

    pub(super) fn diff_index(&self) -> Option<DiffStamp> {
        let stamp = DiffStamp::at(&self.git_dir.as_ref()?.join("index"));
        matches!(stamp, DiffStamp::File { .. }).then_some(stamp)
    }

    fn recount_tracked(&self, known: &mut TrackedDiff, changed: &[usize], against: &str) -> Option<bool> {
        if !self.status.sources.is_empty() || !self.status.unmerged.is_empty() {
            return Some(false);
        }
        let attributes = |path: &Path| {
            path.file_name().is_some_and(|name| name.as_encoded_bytes().eq_ignore_ascii_case(b".gitattributes"))
        };
        // Edited attributes can change the counts of other files too.
        if self.status.untracked.iter().any(|path| attributes(Path::new(path)))
            || self.status.ignored_files.iter().any(|path| attributes(path))
        {
            return Some(false);
        }
        let mut rows = Vec::with_capacity(changed.len());
        for &at in changed {
            let path = &self.status.tracked_uncommitted[at];
            if !matches!(known.stamps[at], DiffStamp::File { .. }) || attributes(Path::new(path)) {
                return Some(false);
            }
            // Neither the old nor new diff can have a rename candidate at an
            // edited path. Additions and deletions can pair across the tree.
            let Some(row) = known.files.iter().position(|file| file.path == *path && file.kind == Kind::Modified)
            else {
                return Some(false);
            };
            rows.push(row);
        }
        let paths = changed.iter().map(|&i| self.status.tracked_uncommitted[i].as_str());
        let raw = self.require(super::run::diff_counts(&self.root, self.guard(), against, "--no-renames", paths))?;
        let files = parse_files(&raw).files;
        if files.len() != rows.len()
            || !files.iter().zip(&rows).all(|(file, &row)| {
                file.kind == Kind::Modified && file.path == known.files[row].path
            })
            // A save or index update during the scoped diff can change which
            // paths need recounting. Don't merge results from two snapshots.
            || !self.diff_stamps().eq(known.stamps.iter().copied())
            || self.diff_index() != known.index
        {
            return Some(false);
        }
        for ((mut file, row), &at) in files.into_iter().zip(rows).zip(changed) {
            file.stamp = known.stamps[at].file();
            known.files[row] = file;
        }
        Some(true)
    }

    fn diff_tracked(&self, stamps: &[DiffStamp], against: &str, named: bool) -> Option<Vec<PendingFile>> {
        let paths = &self.status.tracked_uncommitted;
        let scoped = if named { paths.as_slice() } else { &[] };
        let paths_to_count = scoped.iter().map(String::as_str);
        let raw = self.require(super::run::diff_counts(&self.root, self.guard(), against, "-M", paths_to_count))?;
        let stamps: HashMap<&str, _> = paths.iter().zip(stamps).map(|(p, s)| (p.as_str(), s.file())).collect();
        let mut files = parse_files(&raw).files;
        if named {
            self.add_missing_files(&mut files, &stamps);
        } else {
            // The whole-tree diff does not limit itself to these paths.
            files.retain(|file| stamps.contains_key(file.path.as_str()));
        }
        self.mark_conflicts(&mut files);
        for file in &mut files {
            file.stamp = stamps.get(file.path.as_str()).copied().flatten();
        }
        Some(files)
    }

    fn mark_conflicts(&self, files: &mut Vec<PendingFile>) {
        // Git can report a conflict as an ordinary edit or omit it from the diff.
        for path in &self.status.unmerged {
            match files.iter_mut().find(|f| f.path == *path) {
                Some(file) => file.kind = Kind::Conflict,
                None => files.push(PendingFile::new(path, Kind::Conflict)),
            }
        }
    }

    fn add_missing_files(&self, files: &mut Vec<PendingFile>, stamps: &HashMap<&str, Option<FileStamp>>) {
        // A file put back to what the remote has can still differ from HEAD.
        for path in &self.status.tracked_uncommitted {
            if !self.status.sources.contains(path) && !files.iter().any(|f| f.path == *path) {
                let kind = if stamps[path.as_str()].is_some() { Kind::Modified } else { Kind::Deleted };
                files.push(PendingFile::new(path, kind));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::fs;

    fn tracked_page(repo: &mut Repo) -> PendingFile {
        repo.pending().unwrap().into_iter().find(|f| f.path == "page.txt" && !f.unpushed).unwrap()
    }

    #[test]
    fn unrelated_status_changes_reuse_tracked_line_counts() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        f.write("page.txt", "first\nsecond\n");
        let mut warm = Repo::open(&f.root).unwrap();
        warm.read();
        let page = tracked_page(&mut warm);
        assert_eq!((page.add, page.del), (1, 0));

        f.write("loose.txt", "untracked\n");
        warm.read();
        let mut cold = Repo::open(&f.root).unwrap();
        cold.read();
        // An attempted diff fails, so the successful refresh proves it was skipped.
        fs::rename(f.root.join(".git/objects"), f.root.join(".git/hidden-objects")).unwrap();
        assert!(cold.pending().is_none());
        let files = warm.pending().unwrap();
        assert_eq!(files[0], page);
        assert_eq!((files[1].path.as_str(), files[1].kind), ("loose.txt", Kind::Untracked));
    }

    #[test]
    fn staged_blob_changes_invalidate_counts_and_a_failed_diff_is_retried() {
        let f = Fixture::new();
        f.write("page.txt", "base\n");
        f.write("a.txt", "one staged version\n");
        f.write("b.txt", "another staged version\n");
        f.commit("first");
        f.write("page.txt", "base\nworktree edit\n");
        let mut repo = Repo::open(&f.root).unwrap();
        let stage = |repo: &Repo, object: &str| {
            let oid = repo.git(&["rev-parse", object]).unwrap();
            f.git(&["update-index", "--cacheinfo", "100644", std::str::from_utf8(&oid).unwrap().trim(), "page.txt"]);
        };
        stage(&repo, "HEAD:a.txt");
        repo.read();
        let page = tracked_page(&mut repo);
        let before = repo.raw.clone();

        stage(&repo, "HEAD:b.txt");
        repo.read();
        fn staged_id(raw: &[u8]) -> &[u8] {
            let record = tracked_records(raw).next().unwrap().0;
            assert!(record.starts_with(b"1 MM "));
            record.splitn(9, |&b| b == b' ').nth(7).unwrap()
        }
        assert_ne!(staged_id(&before), staged_id(&repo.raw));
        assert_eq!(page.stamp, FileStamp::at(&f.root.join("page.txt")));

        let objects = f.root.join(".git/objects");
        let hidden = f.root.join(".git/hidden-objects");
        fs::rename(&objects, &hidden).unwrap();
        assert!(repo.pending().is_none(), "staging a different blob must run a new diff");
        assert!(!repo.ok());
        fs::rename(&hidden, &objects).unwrap();
        repo.read();
        assert_eq!(tracked_page(&mut repo), page);
        assert!(repo.ok());
    }

    #[test]
    fn worktree_stamps_and_comparison_base_refresh_the_counts() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.write("peer.txt", "base\n");
        f.commit("first");
        f.push();
        f.write("page.txt", "first\nsecond\n");
        f.commit("second");
        f.write("page.txt", "first\nsecond\nthird\n");
        f.write("peer.txt", "local edit\n");
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        assert_eq!(tracked_page(&mut repo).add, 2);

        let before = repo.raw.clone();
        f.write("page.txt", "first\nsecond\nthird\nfourth\n");
        repo.read();
        assert!(tracked_records(&before).eq(tracked_records(&repo.raw)), "the edit only changed the worktree stamp");
        assert_eq!(tracked_page(&mut repo).add, 3);

        f.push();
        repo.read();
        assert!(tracked_records(&before).eq(tracked_records(&repo.raw)), "the push only changed the comparison base");
        assert_eq!(tracked_page(&mut repo).add, 2);
    }

    #[test]
    fn large_refresh_recounts_only_the_saved_path() {
        let f = Fixture::new();
        let saved = "module/:*.txt";
        f.write(saved, "base\n");
        for i in 0..PATHS_LIMIT {
            f.write(&format!("file{i:03}.txt"), &format!("base {i}\n"));
        }
        f.commit("base");
        f.write(saved, "base\nfirst edit\n");
        for i in 0..PATHS_LIMIT {
            f.write(&format!("file{i:03}.txt"), &format!("base {i}\nlocal edit\n"));
        }
        let mut warm = Repo::open(&f.root).unwrap();
        warm.read();
        let before = warm.pending().unwrap();
        assert_eq!(before.len(), PATHS_LIMIT + 1);
        let raw = warm.raw.clone();

        // Git can still read status and recount the saved file, but a
        // whole-tree recount needs an unrelated file's missing base blob.
        let oid = warm.git(&["rev-parse", "HEAD:file000.txt"]).unwrap();
        let oid = std::str::from_utf8(&oid).unwrap().trim();
        let object = f.root.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
        let hidden = object.with_extension("hidden");
        fs::rename(&object, &hidden).unwrap();
        f.write(saved, "base\nnext edit\nextra line\n");

        warm.read();
        assert!(tracked_records(&raw).eq(tracked_records(&warm.raw)));
        let updated = warm.pending().unwrap();
        assert_eq!(updated.len(), before.len());
        for (old, new) in before.iter().zip(&updated) {
            if old.path == saved {
                assert_eq!(new.path, saved);
                assert_eq!((new.add, new.del), (2, 0));
            } else {
                assert_eq!(new, old);
            }
        }
        let mut cold = Repo::open(&f.root).unwrap();
        cold.read();
        assert!(cold.pending().is_none());
        fs::rename(&hidden, &object).unwrap();
        cold.read();
        assert_eq!(cold.pending().unwrap(), updated);
    }

    #[test]
    fn changed_rename_similarity_uses_the_full_diff() {
        let f = Fixture::new();
        let base = "a recognizable line of text\n".repeat(20);
        f.write("old.txt", &base);
        f.write("peer.txt", "base\n");
        f.commit("base");
        f.git(&["mv", "old.txt", "new.txt"]);
        f.write("new.txt", &format!("{base}one new line\n"));
        f.write("peer.txt", "local edit\n");
        let mut warm = Repo::open(&f.root).unwrap();
        warm.read();
        let before = warm.pending().unwrap();
        assert!(before.iter().any(|file| file.path == "new.txt" && file.kind == Kind::Renamed));
        assert_eq!(warm.moved_from("new.txt").as_deref(), Some("old.txt"));

        let raw = warm.raw.clone();
        f.write("new.txt", &"nothing in common\n".repeat(20));
        warm.read();
        assert!(tracked_records(&raw).eq(tracked_records(&warm.raw)));
        let after = warm.pending().unwrap();
        assert!(after.iter().any(|file| file.path == "old.txt" && file.kind == Kind::Deleted));
        assert!(after.iter().any(|file| file.path == "new.txt" && file.kind == Kind::Added));
        assert_eq!(warm.moved_from("new.txt"), None);
    }

    #[test]
    fn recounting_one_file_includes_its_former_children_but_not_matching_siblings() {
        let f = Fixture::new();
        let path = "src/[a]*?";
        let child = format!("{path}/old");
        let content = "a recognizable line\n".repeat(20);
        f.write(&child, &content);
        f.write("src/abc", "unrelated\n");
        f.commit("base");
        f.push();
        fs::remove_dir_all(f.root.join(path)).unwrap();
        f.write(path, &content);
        f.write("src/abc", "also unpushed\n");
        f.commit("replace directory with its file");
        f.write(path, &(content + "one more line\n"));

        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        let pending = repo.pending().unwrap();
        let mut local = pending.iter().filter(|f| !f.unpushed);
        let file = local.next().unwrap();
        assert_eq!((file.path.as_str(), file.kind, file.from.as_deref()), (path, Kind::Renamed, Some(child.as_str())));
        assert_eq!((file.add, file.del), (1, 0));
        assert!(local.next().is_none());
    }

    #[test]
    fn a_single_path_recount_does_not_report_an_unpushed_backslash_twin_as_dirty() {
        let f = Fixture::new();
        f.write("ab", "original\n");
        f.write(r"a\b", "original twin\n");
        f.commit("base");
        f.push();
        f.write(r"a\b", "committed twin\n");
        f.commit("unpushed twin");
        f.write("ab", "original\nworktree edit\n");

        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        let pending = repo.pending().unwrap();
        let mut local = pending.iter().filter(|file| !file.unpushed);
        let changed = local.next().unwrap();
        assert_eq!((changed.path.as_str(), changed.add, changed.del), ("ab", 1, 0));
        assert!(local.next().is_none());
        assert!(pending.iter().any(|file| file.path == r"a\b" && file.unpushed));
    }

    #[test]
    fn index_update_after_status_does_not_reuse_stale_counts() {
        let f = Fixture::new();
        f.write("page.txt", "base\n");
        f.write("peer.txt", "base\n");
        f.commit("base");
        f.write("page.txt", "base\nfirst edit\n");
        f.write("peer.txt", "base\nlocal edit\n");
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        let before = repo.pending().unwrap();
        assert_eq!((before[1].path.as_str(), before[1].kind, before[1].add), ("peer.txt", Kind::Modified, 1));

        f.write("page.txt", "base\nnext edit\nextra line\n");
        repo.read();
        f.git(&["rm", "--cached", "peer.txt"]);
        let after = repo.pending().unwrap();
        assert_eq!((after[1].path.as_str(), after[1].kind, after[1].del), ("peer.txt", Kind::Deleted, 1));
    }

    #[test]
    fn edit_after_sampling_prevents_merging_scoped_counts() {
        let f = Fixture::new();
        f.write("page.txt", "base\n");
        f.write("peer.txt", "base\n");
        f.commit("base");
        f.write("page.txt", "base\nfirst edit\n");
        f.write("peer.txt", "base\nlocal edit\n");
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        let before = repo.pending().unwrap();
        let mut known = repo.pending_tracked.take().unwrap();

        f.write("page.txt", "base\nnext edit\nextra line\n");
        known.stamps = repo.diff_stamps().collect();
        f.write("peer.txt", "base\nanother edit\nextra line\n");
        assert_eq!(repo.recount_tracked(&mut known, &[0], repo.against().unwrap()), Some(false));
        assert_eq!(known.files, before);
    }

    #[test]
    fn rename_sources_and_conflict_stages_are_part_of_the_dirty_state() {
        let rename = "2 R. N... 100644 100644 100644 h1 h2 R100 new.txt";
        let conflicted = "u UU N... 100644 100644 100644 100644 h1 h2 h3 both.txt";
        let status = format!("{rename}\0? old.txt\0{conflicted}\0? untracked.txt\0");
        let changed_source = status.replace("? old.txt", "? other.txt");
        let changed_conflict = status.replace("h3 both.txt", "h4 both.txt");
        let mut records = tracked_records(status.as_bytes());
        assert_eq!(records.next(), Some((rename.as_bytes(), Some(b"? old.txt".as_slice()))));
        assert_eq!(records.next(), Some((conflicted.as_bytes(), None)));
        assert_eq!(records.next(), None);
        assert!(!tracked_records(status.as_bytes()).eq(tracked_records(changed_source.as_bytes())));
        assert!(!tracked_records(status.as_bytes()).eq(tracked_records(changed_conflict.as_bytes())));
    }

    #[test]
    fn forced_reload_recounts_when_attributes_change() {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first");
        f.write("page.txt", "second\n");
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        assert_eq!(tracked_page(&mut repo).add, 1);

        f.write(".gitattributes", "page.txt -diff\n");
        repo.invalidate_tracked_diff();
        repo.read();
        let page = tracked_page(&mut repo);
        assert_eq!((page.add, page.del), (0, 0));
    }

    #[test]
    fn replacing_a_file_with_the_same_size_and_mtime_refreshes_counts() {
        let f = Fixture::new();
        f.write("page.txt", "aaaa\nbbbb\n");
        f.commit("first");
        f.write("page.txt", "xxxx\nbbbb\n");
        let mut repo = Repo::open(&f.root).unwrap();
        repo.read();
        let page = tracked_page(&mut repo);
        assert_eq!((page.add, page.del), (1, 1));

        let replacement = f.root.join("replacement");
        f.write("replacement", "xxxx\nyyyy\n");
        fs::File::options().write(true).open(&replacement).unwrap().set_modified(page.stamp.unwrap().written).unwrap();
        fs::rename(replacement, f.root.join("page.txt")).unwrap();
        assert!(repo.poll(std::slice::from_ref(&page)), "the replacement kept its size and mtime");
        repo.read();
        let changed = tracked_page(&mut repo);
        assert_eq!(changed.stamp, page.stamp);
        assert_eq!((changed.add, changed.del), (2, 2));
    }
}
