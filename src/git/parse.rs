//! Turns git's status, diff and log output into the repository's data.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use super::{Commit, CommitId, CommitVersion, DiffFiles, Ends, Kind, PendingFile};
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
    pub(super) ignored_dirs: Vec<PathBuf>,
    pub(super) ignored_files: Vec<PathBuf>,
    /// Paths left in conflict by a merge.
    pub(super) unmerged: Vec<String>,
    /// Changed entries that cannot be opened using the panes' UTF-8 paths.
    pub(super) unrepresented: usize,
}

pub(super) fn utf8_path(raw: &[u8]) -> Option<&str> {
    std::str::from_utf8(raw).ok().filter(|path| !path.is_empty())
}

fn status_path(record: &[u8], fields: usize) -> Option<&str> {
    utf8_path(record.splitn(fields, |&b| b == b' ').nth(fields - 1)?)
}

/// Parses `git status --porcelain=v2 --branch -z`.
pub(super) fn parse_status(raw: &[u8]) -> Status {
    let mut st = Status::default();
    let mut entries = raw.split(|&b| b == 0);
    // Git names a missing upstream but omits the ahead/behind line.
    let mut measured = false;
    // The path is the last of a fixed number of fields, and may hold spaces.
    while let Some(bytes) = entries.next() {
        if let Some(path) = bytes.strip_prefix(b"! ") {
            // Don't accidentally exclude a different path with a replacement character.
            let decode = |bytes: &[u8]| PathBuf::from(OsStr::from_bytes(bytes));
            if let Some(dir) = path.strip_suffix(b"/") {
                st.ignored_dirs.push(decode(dir));
            } else {
                st.ignored_files.push(decode(path));
            }
            continue;
        }
        if let Some(path) = bytes.strip_prefix(b"? ") {
            // A nested repository is reported as a directory, with a slash.
            if let Some(path) = utf8_path(path) {
                st.untracked.push(path.trim_end_matches('/').to_owned());
            } else {
                st.unrepresented += 1;
            }
        } else if bytes.starts_with(b"1 ") {
            if let Some(path) = status_path(bytes, 9) {
                st.tracked_uncommitted.push(path.to_owned());
            } else {
                st.unrepresented += 1;
            }
        } else if bytes.starts_with(b"2 ") {
            let mut fields = bytes.splitn(10, |&b| b == b' ');
            let rename = fields.nth(8).is_some_and(|score| score.starts_with(b"R"));
            let path = fields.next().and_then(utf8_path);
            // Both renames and copies have another token. A copy doesn't
            // require the source to be openable because it is an addition.
            let source = entries.next().and_then(utf8_path);
            if let Some(path) = path.filter(|_| !rename || source.is_some()) {
                st.tracked_uncommitted.push(path.to_owned());
                if let Some(source) = source.filter(|_| rename) {
                    st.tracked_uncommitted.push(source.to_owned());
                    st.sources.push(source.to_owned());
                }
            } else {
                st.unrepresented += 1;
            }
        } else if bytes.starts_with(b"u ") {
            if let Some(path) = status_path(bytes, 11) {
                st.unmerged.push(path.to_owned());
                st.tracked_uncommitted.push(path.to_owned());
            } else {
                st.unrepresented += 1;
            }
        } else if let Some(v) = bytes.strip_prefix(b"# branch.oid ") {
            st.ends.tip = String::from_utf8_lossy(v).into_owned();
        } else if let Some(v) = bytes.strip_prefix(b"# branch.head ") {
            st.branch = String::from_utf8_lossy(v).into_owned();
        } else if let Some(v) = bytes.strip_prefix(b"# branch.upstream ") {
            st.upstream = Some(String::from_utf8_lossy(v).into_owned());
        } else if let Some(v) = bytes.strip_prefix(b"# branch.ab +") {
            measured = true;
            st.ahead = v
                .split(|&b| b == b' ')
                .next()
                .and_then(|n| std::str::from_utf8(n).ok()?.parse().ok())
                .unwrap_or(0);
        }
    }
    if !measured {
        st.gone = st.upstream.take();
    }
    st
}

fn diff_kind(code: u8) -> Kind {
    match code {
        b'A' | b'C' => Kind::Added,
        b'D' => Kind::Deleted,
        b'U' => Kind::Conflict,
        b'R' => Kind::Renamed,
        _ => Kind::Modified,
    }
}

/// Finds the exact destination path in `--name-status -z` output, or fails
/// if opening that change would require an unrepresentable rename source.
pub(super) fn changed_file(raw: &[u8], path: &str) -> Option<Option<PendingFile>> {
    if raw.is_empty() {
        return Some(None);
    }
    let mut tokens = raw.strip_suffix(b"\0")?.split(|&b| b == 0);
    while let Some(status) = tokens.next() {
        let code = *status.first()?;
        // Renames and copies list the old path first.
        let old = if matches!(code, b'R' | b'C') { Some(tokens.next()?) } else { None };
        if tokens.next()? == path.as_bytes() {
            let mut file = PendingFile::new(path, diff_kind(code));
            if code == b'R' {
                file.from = Some(utf8_path(old?)?.to_owned());
            }
            return Some(Some(file));
        }
    }
    Some(None)
}

/// Parses `--raw --numstat -z` output: what happened to each path, then how
/// many lines each gained and lost.
pub(super) fn parse_files(raw: &[u8]) -> DiffFiles {
    let mut diff = DiffFiles::default();
    let mut tokens = raw.split(|&b| b == 0).filter(|t| !t.is_empty());
    let mut next = 0;
    let number = |s: &[u8]| std::str::from_utf8(s).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    while let Some(tok) = tokens.next() {
        if tok.starts_with(b":") {
            let code = tok.rsplit(|&b| b == b' ').next().and_then(|c| c.first()).copied().unwrap_or(b'M');
            let kind = diff_kind(code);
            // Renames and copies list the old path first.
            let old = if matches!(code, b'R' | b'C') { tokens.next().and_then(utf8_path) } else { None };
            if let Some(path) = tokens.next() {
                if let Some(path) = utf8_path(path).filter(|_| code != b'R' || old.is_some()) {
                    let mut file = PendingFile::new(path, kind);
                    file.from = old.filter(|_| code == b'R').map(str::to_owned);
                    diff.files.push(file);
                } else {
                    diff.unrepresented += 1;
                }
            }
            continue;
        }
        let mut f = tok.splitn(3, |&b| b == b'\t');
        let (Some(add), Some(del), Some(path)) = (f.next(), f.next(), f.next()) else { continue };
        let (old, path) = if path.is_empty() {
            (tokens.next(), tokens.next().unwrap_or_default())
        } else {
            (None, path)
        };
        let Some(path) = utf8_path(path) else { continue };
        // Both lists come in the same order, so the match is normally `next`.
        // The old path also matters for a rename whose source cannot be opened.
        let matches = |file: &PendingFile| {
            file.path == path && (file.kind != Kind::Renamed || file.from.as_deref().map(str::as_bytes) == old)
        };
        let at = if diff.files.get(next).is_some_and(matches) {
            Some(next)
        } else {
            diff.files.iter().position(matches)
        };
        if let Some(i) = at {
            next = i + 1;
            // Binary files report "-" and keep their counts at zero.
            diff.files[i].add = number(add);
            diff.files[i].del = number(del);
        }
    }
    diff
}

/// A commit's time as `%ct` prints it: the digits the commit recorded. Git
/// prints none for a commit without a date, and shows the start of 1970 for
/// that and for a date too large to hold.
fn commit_time(digits: &[u8]) -> Option<i64> {
    let digits = std::str::from_utf8(digits).ok().filter(|text| text.bytes().all(|b| b.is_ascii_digit()))?;
    Some(digits.parse().unwrap_or(0))
}

pub(super) fn commit_version(raw: &[u8], id: CommitId<'_>) -> Option<CommitVersion> {
    let (hash, time) = std::str::from_utf8(raw).ok()?.trim_end().split_once('\0')?;
    // Git also accepts refs made of hex digits. Match the object name, not the ref name.
    hash.get(..id.0.len())?.eq_ignore_ascii_case(id.0).then_some(())?;
    Some(CommitVersion { hash: hash.to_string(), time: commit_time(time.as_bytes())? })
}

/// How much of a commit's subject is kept. Git joins a whole first paragraph
/// into the subject, and the panes measure every subject they draw.
const SUBJECT_LIMIT: usize = 4096;

/// The subject as text, cut after a whole character and marked when it was longer.
fn subject(raw: &[u8]) -> String {
    if raw.len() <= SUBJECT_LIMIT {
        return String::from_utf8_lossy(raw).into_owned();
    }
    let mut end = SUBJECT_LIMIT;
    // A character's later bytes all start with the bits 10.
    while end > SUBJECT_LIMIT - 3 && raw[end] & 0xc0 == 0x80 {
        end -= 1;
    }
    let mut kept = String::from_utf8_lossy(&raw[..end]).into_owned();
    kept.push('…');
    kept
}

/// Parses history and marks the commits in the unpushed list.
pub(super) fn parse_history(log: &[u8], unpushed: &[u8]) -> Option<Vec<Commit>> {
    let unpushed: HashSet<&[u8]> = unpushed.split(|&b| b == b'\n').filter(|oid| !oid.is_empty()).collect();
    let mut commits = Vec::new();
    if !log.is_empty() {
        let mut fields = log.strip_suffix(b"\0")?.split(|&b| b == 0);
        while let Some(hash) = fields.next() {
            let name = std::str::from_utf8(hash).ok()?;
            let time = commit_time(fields.next()?)?;
            let parents = fields.next()?;
            let title = fields.next()?;
            commits.push(Commit {
                unpushed: unpushed.contains(hash),
                hash: name.to_owned(),
                subject: subject(title),
                time,
                day: when::local_day(time),
                merge: parents.contains(&b' '),
            });
        }
    }
    Some(commits)
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
    fn status_separates_ignored_paths_from_uncommitted_files() {
        let st = parse_status(
            b"! cache with spaces/\0! raw-\xff/\0! build log\0! raw-\xfe.log\0? scratch.txt\0\
              1 .M N... 100644 100644 100644 h1 h2 source.rs\0",
        );
        assert_eq!(st.ignored_dirs[0], PathBuf::from("cache with spaces"));
        assert_eq!(st.ignored_dirs[1].as_os_str().as_bytes(), b"raw-\xff");
        assert_eq!(st.ignored_dirs.len(), 2);
        assert_eq!(st.ignored_files[0], PathBuf::from("build log"));
        assert_eq!(st.ignored_files[1].as_os_str().as_bytes(), b"raw-\xfe.log");
        assert_eq!(st.ignored_files.len(), 2);
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
        assert_eq!(st.sources, ["? old looks untracked"]);
        assert_eq!(st.untracked, ["real.txt"]);
    }

    #[test]
    fn status_keeps_a_modified_copy_source_independent() {
        let st = parse_status(
            b"1 MM N... 100644 100644 100644 h1 h2 ? original\0\
              2 C. N... 100644 100644 100644 h1 h1 C100 copy\0? original\0? loose.txt\0",
        );
        assert_eq!(st.tracked_uncommitted, ["? original", "copy"]);
        assert!(st.sources.is_empty());
        assert_eq!(st.untracked, ["loose.txt"]);
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
        )
        .files;
        let got: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.kind, f.add, f.del)).collect();
        assert_eq!(
            got,
            [("src/a.rs", Kind::Modified, 3, 1), ("new file.txt", Kind::Added, 5, 0), ("gone", Kind::Deleted, 0, 7)]
        );
    }

    #[test]
    fn diff_keeps_the_source_of_a_rename() {
        let files =
            parse_files(b":100644 100644 a b R090\0old/name.rs\0new/name.rs\x002\t1\t\0old/name.rs\0new/name.rs\0")
                .files;
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].path.as_str(), files[0].kind), ("new/name.rs", Kind::Renamed));
        assert_eq!(files[0].from.as_deref(), Some("old/name.rs"));
        assert_eq!((files[0].add, files[0].del), (2, 1));
    }

    #[test]
    fn binary_files_have_no_line_counts() {
        let files = parse_files(b":100644 100644 a b M\0logo.png\0-\t-\tlogo.png\0").files;
        assert_eq!((files[0].add, files[0].del), (0, 0));
    }

    #[test]
    fn a_diff_with_a_missing_path_produces_no_file() {
        assert!(parse_files(b":100644 100644 a b \0").files.is_empty());
    }

    #[test]
    fn invalid_path_bytes_do_not_alias_a_replacement_character() {
        let st = parse_status(
            b"? path\xff\0? path\xef\xbf\xbd\0\
              2 R. N... 100644 100644 100644 h1 h2 R100 new.txt\0old\xff\0? last.txt\0",
        );
        assert_eq!(st.untracked, ["path\u{fffd}", "last.txt"]);
        assert!(st.tracked_uncommitted.is_empty());
        assert_eq!(st.unrepresented, 2);

        let diff = parse_files(
            b":100644 100644 a b M\0path\xff\0:100644 100644 a b M\0path\xef\xbf\xbd\0\
              100\t50\tpath\xff\x002\t1\tpath\xef\xbf\xbd\0",
        );
        assert_eq!(diff.unrepresented, 1);
        assert_eq!(diff.files.len(), 1);
        let file = &diff.files[0];
        assert_eq!((file.path.as_str(), file.add, file.del), ("path\u{fffd}", 2, 1));
        assert!(changed_file(b"R100\0old\xff\0new.txt\0", "new.txt").is_none());
    }

    #[test]
    fn history_fields_are_nul_delimited() {
        let commits = parse_history(b"aaaa\x0017\0\0first\nline\0bbbb\x0018\0aaaa cccc\0\0", b"bbbb\n").unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!((commits[0].subject.as_str(), commits[0].time), ("first\nline", 17));
        assert!(commits[1].subject.is_empty() && commits[1].unpushed && commits[1].merge);
        assert!(parse_history(b"aaaa\0invalid\0\0subject\0", b"").is_none());
    }

    #[test]
    fn a_commit_without_a_usable_date_keeps_its_place_in_history() {
        // Git prints no digits for a commit that records no date, and all of them for one out of range.
        let commits =
            parse_history(b"aaaa\x0017\0\0newer\0bbbb\0\0aaaa\0undated\0cccc\x0099999999999999999999\0\0huge\0", b"");
        let times: Vec<_> = commits.unwrap().iter().map(|c| (c.subject.clone(), c.time)).collect();
        assert_eq!(times, [("newer".to_owned(), 17), ("undated".to_owned(), 0), ("huge".to_owned(), 0)]);
        assert_eq!(commit_version(b"bbbb\0\n", CommitId::parse("bbbb").unwrap()).map(|c| c.time), Some(0));
        assert!(commit_version(b"bbbb\0soon\n", CommitId::parse("bbbb").unwrap()).is_none());
    }

    #[test]
    fn a_subject_longer_than_the_panes_measure_is_cut_at_a_character_and_says_so() {
        let log = |subject: &str| format!("aaaa\x0017\0\0{subject}\0").into_bytes();
        let kept = |subject: &str| parse_history(&log(subject), b"").unwrap().remove(0).subject;
        let whole = "a".repeat(SUBJECT_LIMIT);
        assert_eq!(kept(&whole), whole);
        // The limit falls inside the two bytes of `é`.
        let long = format!("{}é{}", "a".repeat(SUBJECT_LIMIT - 1), "tail ".repeat(1 << 16));
        assert_eq!(kept(&long), format!("{}…", "a".repeat(SUBJECT_LIMIT - 1)));
    }
}
