//! Turns git's status, diff and log output into the repository's data.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use super::{Commit, CommitId, CommitVersion, Ends, Kind, PendingFile};
use crate::when;

#[derive(Default)]
pub(super) struct Status {
    pub(super) branch: String,
    pub(super) ends: Ends,
    /// The upstream this branch is measured against.
    pub(super) upstream: Option<String>,
    /// A named upstream whose branch is missing.
    pub(super) gone: Option<String>,
    pub(super) ahead: u32,
    /// Includes both paths of a move.
    pub(super) tracked_uncommitted: Vec<String>,
    /// Where the moved ones among them were before.
    pub(super) sources: Vec<String>,
    pub(super) untracked: Vec<String>,
    /// Directories git ignores whole; none of their contents are listed.
    pub(super) ignored: Vec<PathBuf>,
    /// Paths left in conflict by a merge.
    pub(super) unmerged: Vec<String>,
}

/// Parses `git status --porcelain=v2 --branch -z`.
pub(super) fn parse_status(raw: &[u8]) -> Status {
    let mut st = Status::default();
    let mut entries = raw.split(|&b| b == 0);
    // Git names a missing upstream but omits the ahead/behind line.
    let mut measured = false;
    // The path is the last of a fixed number of fields, and may hold spaces.
    let path = |entry: &str, fields: usize| entry.splitn(fields, ' ').nth(fields - 1).map(str::to_string);
    while let Some(bytes) = entries.next() {
        if let Some(dir) = bytes.strip_prefix(b"! ").and_then(|path| path.strip_suffix(b"/")) {
            // Don't accidentally exclude a different path with a replacement character.
            st.ignored.push(PathBuf::from(OsStr::from_bytes(dir)));
            continue;
        }
        let text = String::from_utf8_lossy(bytes);
        let e = text.as_ref();
        if let Some(path) = e.strip_prefix("? ") {
            // A nested repository is reported as a directory, with a slash.
            st.untracked.push(path.trim_end_matches('/').to_string());
        } else if e.starts_with("1 ") {
            st.tracked_uncommitted.extend(path(e, 9));
        } else if e.starts_with("2 ") {
            // A rename is followed by its old path, which is not an entry.
            st.tracked_uncommitted.extend(path(e, 10));
            let from = String::from_utf8_lossy(entries.next().unwrap_or_default());
            st.tracked_uncommitted.push(from.to_string());
            st.sources.push(from.into_owned());
        } else if e.starts_with("u ") {
            st.unmerged.extend(path(e, 11));
            st.tracked_uncommitted.extend(path(e, 11));
        } else if let Some(v) = e.strip_prefix("# branch.oid ") {
            st.ends.tip = v.to_string();
        } else if let Some(v) = e.strip_prefix("# branch.head ") {
            st.branch = v.to_string();
        } else if let Some(v) = e.strip_prefix("# branch.upstream ") {
            st.upstream = Some(v.to_string());
        } else if let Some(v) = e.strip_prefix("# branch.ab +") {
            measured = true;
            st.ahead = v.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        }
    }
    if !measured {
        st.gone = st.upstream.take();
    }
    st
}

/// Parses `--raw --numstat -z` output: what happened to each path, then how
/// many lines each gained and lost.
pub(super) fn parse_files(raw: &[u8]) -> Vec<PendingFile> {
    let mut files = Vec::new();
    let text = String::from_utf8_lossy(raw);
    let mut tokens = text.split('\0').filter(|t| !t.is_empty());
    let mut next = 0;
    while let Some(tok) = tokens.next() {
        if tok.starts_with(':') {
            let code = tok.rsplit(' ').next().and_then(|c| c.bytes().next()).unwrap_or(b'M');
            let kind = match code {
                b'A' | b'C' => Kind::Added,
                b'D' => Kind::Deleted,
                b'U' => Kind::Conflict,
                b'R' => Kind::Renamed,
                _ => Kind::Modified,
            };
            // Renames and copies list the old path first.
            let old = if matches!(code, b'R' | b'C') { tokens.next() } else { None };
            if let Some(path) = tokens.next() {
                let mut file = PendingFile::new(path, kind);
                file.from = old.filter(|_| code == b'R').map(str::to_string);
                files.push(file);
            }
            continue;
        }
        let mut f = tok.splitn(3, '\t');
        let (add, del, path) = (f.next().unwrap_or(""), f.next().unwrap_or(""), f.next().unwrap_or(""));
        let path = if path.is_empty() {
            tokens.next();
            tokens.next().unwrap_or("")
        } else {
            path
        };
        // Both lists come in the same order, so the match is normally `next`.
        let at = if files.get(next).is_some_and(|f| f.path == path) {
            Some(next)
        } else {
            files.iter().position(|f| f.path == path)
        };
        if let Some(i) = at {
            next = i + 1;
            // Binary files report "-" and keep their counts at zero.
            files[i].add = add.parse().unwrap_or(0);
            files[i].del = del.parse().unwrap_or(0);
        }
    }
    files
}

pub(super) fn commit_version(raw: &[u8], id: CommitId<'_>) -> Option<CommitVersion> {
    let (hash, time) = std::str::from_utf8(raw).ok()?.trim_end().split_once('\0')?;
    // Git also accepts refs made of hex digits. Match the object name, not the ref name.
    hash.get(..id.0.len())?.eq_ignore_ascii_case(id.0).then_some(())?;
    Some(CommitVersion { hash: hash.to_string(), time: time.parse().ok()? })
}

/// Parses history and marks the commits in the unpushed list.
pub(super) fn parse_history(log: &[u8], unpushed: &[u8]) -> Vec<Commit> {
    let unpushed = String::from_utf8_lossy(unpushed);
    let unpushed: HashSet<&str> = unpushed.lines().collect();
    String::from_utf8_lossy(log)
        .lines()
        .filter_map(|line| {
            let mut f = line.splitn(4, '\0');
            let (hash, time, parents, subject) = (f.next()?, f.next()?, f.next()?, f.next()?);
            let time = time.parse().unwrap_or(0);
            Some(Commit {
                unpushed: unpushed.contains(hash),
                hash: hash.to_string(),
                subject: subject.to_string(),
                time,
                day: when::local_day(time),
                merge: parents.contains(' '),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_branch_and_untracked() {
        let st = parse_status(
            b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0? a b.txt\0? nested/\0",
        );
        assert_eq!((st.branch.as_str(), st.ends.tip.as_str(), st.ahead), ("main", "abc", 2));
        assert_eq!(st.upstream.as_deref(), Some("origin/main"));
        assert_eq!(st.untracked, ["a b.txt", "nested"]);
    }

    #[test]
    fn status_separates_ignored_directories_from_uncommitted_files() {
        let st = parse_status(
            b"! cache with spaces/\0! raw-\xff/\0! build.log\0? scratch.txt\0\
              1 .M N... 100644 100644 100644 h1 h2 source.rs\0",
        );
        assert_eq!(st.ignored[0], PathBuf::from("cache with spaces"));
        assert_eq!(st.ignored[1].as_os_str().as_bytes(), b"raw-\xff");
        assert_eq!(st.ignored.len(), 2);
        assert_eq!(st.untracked, ["scratch.txt"]);
        assert_eq!(st.tracked_uncommitted, ["source.rs"]);
        assert!(st.unmerged.is_empty());
    }

    #[test]
    fn status_sets_aside_an_upstream_that_is_gone() {
        let st = parse_status(b"# branch.oid abc\0# branch.head topic\0# branch.upstream gh/topic\0");
        assert_eq!((st.upstream, st.gone.as_deref()), (None, Some("gh/topic")));
    }

    #[test]
    fn status_reads_conflicts_and_skips_rename_sources() {
        let st = parse_status(
            b"u UU N... 100644 100644 100644 100644 h1 h2 h3 both sides.txt\0\
              2 R. N... 100644 100644 100644 h1 h2 R100 new.txt\0? old looks untracked\0? real.txt\0",
        );
        assert_eq!(st.unmerged, ["both sides.txt"]);
        assert_eq!(st.untracked, ["real.txt"]);
    }

    #[test]
    fn status_lists_every_tracked_path_that_differs() {
        let st = parse_status(
            b"1 .M N... 100644 100644 100644 h1 h2 src/a b.rs\0\
              2 R. N... 100644 100644 100644 h1 h2 R100 new name.txt\0old name.txt\0\
              u UU N... 100644 100644 100644 100644 h1 h2 h3 both.txt\0? loose.txt\0",
        );
        assert_eq!(st.tracked_uncommitted, ["src/a b.rs", "new name.txt", "old name.txt", "both.txt"]);
    }

    #[test]
    fn diff_pairs_kinds_with_counts() {
        let files = parse_files(
            b":100644 100644 a b M\0src/a.rs\0:000000 100644 0 c A\0new file.txt\0:100644 000000 d 0 D\0gone\0\
              3\t1\tsrc/a.rs\x005\t0\tnew file.txt\x000\t7\tgone\0",
        );
        let got: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.kind, f.add, f.del)).collect();
        assert_eq!(
            got,
            [("src/a.rs", Kind::Modified, 3, 1), ("new file.txt", Kind::Added, 5, 0), ("gone", Kind::Deleted, 0, 7)]
        );
    }

    #[test]
    fn diff_keeps_the_source_of_a_rename() {
        let files =
            parse_files(b":100644 100644 a b R090\0old/name.rs\0new/name.rs\x002\t1\t\0old/name.rs\0new/name.rs\0");
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].path.as_str(), files[0].kind), ("new/name.rs", Kind::Renamed));
        assert_eq!(files[0].from.as_deref(), Some("old/name.rs"));
        assert_eq!((files[0].add, files[0].del), (2, 1));
    }

    #[test]
    fn binary_files_have_no_line_counts() {
        let files = parse_files(b":100644 100644 a b M\0logo.png\0-\t-\tlogo.png\0");
        assert_eq!((files[0].add, files[0].del), (0, 0));
    }

    #[test]
    fn a_diff_with_a_missing_path_produces_no_file() {
        assert!(parse_files(b":100644 100644 a b \0").is_empty());
    }
}
