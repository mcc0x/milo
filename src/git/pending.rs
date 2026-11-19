//! Caches the line counts of the tracked changes in a status snapshot.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::{EMPTY_TREE, FileStamp, Kind, PATHS_LIMIT, PendingFile, Repo, parse_files};

/// Worktree inputs that can be stamped without opening a file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffStamp {
    Missing,
    File { stamp: FileStamp, device: u64, inode: u64, changed: (i64, i64) },
    Uncertain(Option<FileStamp>),
}

impl DiffStamp {
    fn at(path: &Path) -> Self {
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

#[derive(Default)]
pub(super) struct TrackedDiff {
    /// In status path order, including both ends of a rename.
    stamps: Vec<DiffStamp>,
    files: Vec<PendingFile>,
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

    pub(super) fn tracked_uncommitted(&mut self) -> Option<Vec<PendingFile>> {
        let paths = &self.status.tracked_uncommitted;
        if paths.is_empty() {
            return Some(Vec::new());
        }
        // Sample before Git runs; an edit during the diff must not be cached
        // under a stamp taken after the edit. Take the old entry so a failed
        // diff cannot leave stale results behind.
        let mut known = self.pending_tracked.take().unwrap_or_default();
        let mut fresh = known.stamps.len() == paths.len();
        let mut cacheable = true;
        let mut absolute = PathBuf::new();
        known.stamps.resize(paths.len(), DiffStamp::Missing);
        for (path, previous) in paths.iter().zip(&mut known.stamps) {
            absolute.clear();
            absolute.push(&self.root);
            absolute.push(path);
            let stamp = DiffStamp::at(&absolute);
            fresh &= stamp == *previous;
            // A lossy status path may not name the file we just stamped.
            cacheable &= !matches!(stamp, DiffStamp::Uncertain(_)) && !path.contains('\u{fffd}');
            *previous = stamp;
        }
        if !fresh {
            known.files = self.diff_tracked(&known.stamps)?;
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

    fn diff_tracked(&self, stamps: &[DiffStamp]) -> Option<Vec<PendingFile>> {
        let paths = &self.status.tracked_uncommitted;
        let named = paths.len() <= PATHS_LIMIT;
        // Naming the paths lets the outline use the Reader's base without
        // including files that are only unpushed. Use the tip from status,
        // not a possibly newer HEAD.
        let ends = self.ends();
        let tip = if ends.has_commit() { ends.tip.as_str() } else { EMPTY_TREE };
        let against = if named { ends.base.as_deref().unwrap_or(tip) } else { tip };
        let args = ["diff", against, "-M", "--raw", "--numstat", "-z", "--"];
        let raw = if named {
            let mut scoped = Vec::with_capacity(args.len() + paths.len());
            scoped.extend(args);
            scoped.extend(paths.iter().map(String::as_str));
            self.git(&scoped)?
        } else {
            self.git(&args)?
        };
        let stamps: HashMap<&str, _> = paths.iter().zip(stamps).map(|(p, s)| (p.as_str(), s.file())).collect();
        let mut files = parse_files(&raw);
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
        f.commit("first");
        f.push();
        f.write("page.txt", "first\nsecond\n");
        f.commit("second");
        f.write("page.txt", "first\nsecond\nthird\n");
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
        repo.read();
        let changed = tracked_page(&mut repo);
        assert_eq!(changed.stamp, page.stamp);
        assert_eq!((changed.add, changed.del), (2, 2));
    }
}
