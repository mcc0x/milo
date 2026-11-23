//! The only path from an explicit working-Markdown selection to tkmp.

use std::ffi::OsStr;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use crate::native::trusted::{find, safe_path_text};
use crate::{fs_ops, native, rex};

const MAX_BYTES: u64 = 512 * 1024;
const TOO_LARGE: &str = "Preview needs Markdown no larger than 512 KiB";
const NOT_REGULAR: &str = "Preview needs a regular working file, not a link";
const NOT_TEXT: &str = "Preview needs UTF-8 Markdown without terminal controls";
const UNREADABLE: &str = "Can't read the selected Markdown file";
const CHANGED: &str = "The selected Markdown file changed; try Preview again";
pub(crate) const MISSING_TOOL: &str =
    "Install tkmp outside the checkout and current directory, then add its absolute directory to PATH";

/// Menu eligibility is metadata-only. Availability and contents are checked on explicit input.
pub(crate) fn eligible(root: &Path, root_id: Option<(u64, u64)>, relative: &Path) -> bool {
    markdown_entry(root, root_id, relative).is_ok()
}

/// Availability is a cheap preflight for an explicit menu open, not a Rex probe.
pub(crate) fn availability(root: &Path) -> Result<(), &'static str> {
    available_tool(root).map(|_| ()).map_err(|reason| match reason {
        Unavailable::Rex => "tkmp needs local Rex",
        Unavailable::Tool => "tkmp not installed",
    })
}

enum Unavailable {
    Rex,
    Tool,
}

fn available_tool(root: &Path) -> Result<PathBuf, Unavailable> {
    if !rex::preview_available(root) {
        return Err(Unavailable::Rex);
    }
    installed(Some(root)).ok_or(Unavailable::Tool)
}

pub(crate) fn open(root: &Path, root_id: Option<(u64, u64)>, relative: &Path) -> Result<(), &'static str> {
    let entry = markdown_entry(root, root_id, relative)?;
    let tool = available_tool(root).map_err(|reason| match reason {
        Unavailable::Rex => "Open Milo in a local Rex pane to preview Markdown",
        Unavailable::Tool => MISSING_TOOL,
    })?;
    check_text(&entry)?;
    let current = markdown_entry(root, root_id, relative)?;
    if entry_key(&entry) != entry_key(&current) {
        return Err(CHANGED);
    }
    // Rex and tkmp need a pathname, not our descriptor. The checks above apply
    // at launch; tkmp opens that path again, including on every subsequent save.
    rex::preview(root, &tool, &root.join(relative))
}

fn entry_key(entry: &native::WorkingEntry) -> (libc::dev_t, libc::ino_t, i128, i128) {
    let stat = &entry.stat;
    (stat.st_dev, stat.st_ino, stat.st_ctime.into(), stat.st_ctime_nsec.into())
}

fn markdown_entry(
    root: &Path,
    root_id: Option<(u64, u64)>,
    relative: &Path,
) -> Result<native::WorkingEntry, &'static str> {
    if !relative.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")) {
        return Err("Preview is only available for Markdown files");
    }
    if !safe_path_text(root) || !safe_path_text(relative) {
        return Err("Preview needs a UTF-8 path without terminal controls");
    }
    if relative.components().any(|part| matches!(part, Component::Normal(name) if name.eq_ignore_ascii_case(".git"))) {
        return Err("Preview is only available for files in the working tree");
    }
    let entry = native::working_entry(root, root_id, relative)?;
    if entry.stat.st_mode & libc::S_IFMT != libc::S_IFREG || entry.stat.st_nlink != 1 {
        return Err(NOT_REGULAR);
    }
    #[cfg(target_os = "macos")]
    // SF_DATALESS: Don't hydrate an offline file to preview it.
    if entry.stat.st_flags & 0x4000_0000 != 0 {
        return Err("Download the selected Markdown file locally before previewing it");
    }
    if !u64::try_from(entry.stat.st_size).is_ok_and(|size| size <= MAX_BYTES) {
        return Err(TOO_LARGE);
    }
    Ok(entry)
}

fn check_text(entry: &native::WorkingEntry) -> Result<(), &'static str> {
    let file = fs_ops::open_at(&entry.dir, &entry.name, libc::O_RDONLY | libc::O_NOCTTY, 0).map_err(|_| UNREADABLE)?;
    let metadata = file.metadata().map_err(|_| UNREADABLE)?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || u64::try_from(entry.stat.st_dev).ok() != Some(metadata.dev())
        || entry.stat.st_ino != metadata.ino()
    {
        return Err(CHANGED);
    }
    if metadata.len() > MAX_BYTES {
        return Err(TOO_LARGE);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len() + 1).unwrap());
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|_| UNREADABLE)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(TOO_LARGE);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| NOT_TEXT)?;
    if text.chars().any(|ch| ch.is_control() && !matches!(ch, '\t' | '\r' | '\n')) || encoded_control(text) {
        return Err(NOT_TEXT);
    }
    Ok(())
}

// tkmp writes rendered text and OSC 8 link destinations to the terminal.
// CommonMark can expand numeric entities there, so check those as well as
// literal controls. This is not a sandbox for a live, external viewer.
fn encoded_control(text: &str) -> bool {
    text.match_indices("&#").any(|(at, _)| {
        let tail = &text[at + 2..];
        let (digits, base) = tail.strip_prefix(['x', 'X']).map_or((tail, 10), |hex| (hex, 16));
        let limit = if base == 16 { 6 } else { 7 };
        let end = digits
            .bytes()
            .take(limit + 1)
            .take_while(|&b| if base == 16 { b.is_ascii_hexdigit() } else { b.is_ascii_digit() })
            .count();
        end != 0
            && end <= limit
            && digits.as_bytes().get(end) == Some(&b';')
            && u32::from_str_radix(&digits[..end], base).ok().and_then(char::from_u32).is_some_and(char::is_control)
    })
}

/// Doctor and the action use the same read-only detector; it never runs a candidate.
pub(crate) fn installed(root: Option<&Path>) -> Option<PathBuf> {
    installed_on(std::env::var_os("PATH")?.as_os_str(), root, Path::new("."))
}

fn installed_on(path: &OsStr, root: Option<&Path>, cwd: &Path) -> Option<PathBuf> {
    find("tkmp", root, cwd, None, path)
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn only_bounded_regular_working_markdown_is_eligible() {
        let fixture = Fixture::folder();
        let root = &fixture.root;
        let id = native::identity(root);
        let path = "docs/-two words ' $(draft) 終.MarkDown";
        fixture.write(path, "# A heading\r\n\r\n\t* text\n");
        fixture.write("docs/plain.md", "a regular file");
        fixture.write("docs/plain.txt", "not Markdown");
        fixture.write(".Git/internal.md", "not in the working tree");
        fs::create_dir(root.join("folder.md")).unwrap();
        symlink(path, root.join("leaf.md")).unwrap();
        symlink("docs", root.join("linked-dir")).unwrap();
        fs::hard_link(root.join(path), root.join("hard.md")).unwrap();
        let pipe = CString::new(root.join("fifo.md").as_os_str().as_bytes()).unwrap();
        // SAFETY: The path is a live terminated string in the test fixture.
        assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);

        for bad in [
            "", "docs/plain.txt", "folder.md", "leaf.md", "linked-dir/plain.md", "hard.md", "fifo.md", "absent.md",
            ".Git/internal.md", "docs/../docs/plain.md", "docs/./plain.md", "docs//plain.md", "/docs/plain.md",
        ] {
            assert!(!eligible(root, id, Path::new(bad)), "{bad}");
        }
        assert!(!eligible(root, id, Path::new(path)), "a multiply linked entry is not eligible");
        fs::remove_file(root.join("hard.md")).unwrap();
        assert!(eligible(root, id, Path::new(path)));
        assert!(check_text(&markdown_entry(root, id, Path::new(path)).unwrap()).is_ok());
        assert!(!eligible(root, None, Path::new(path)));

        let large = fs::File::create(root.join("large.md")).unwrap();
        large.set_len(MAX_BYTES).unwrap();
        assert!(eligible(root, id, Path::new("large.md")));
        large.set_len(MAX_BYTES + 1).unwrap();
        assert!(!eligible(root, id, Path::new("large.md")));

        fs::rename(root, root.with_extension("moved")).unwrap();
        fixture.write(path, "replacement");
        assert!(!eligible(root, id, Path::new(path)));
    }

    #[test]
    fn launch_reads_only_bounded_utf8_without_terminal_controls() {
        let fixture = Fixture::folder();
        let root = &fixture.root;
        let id = native::identity(root);
        for bad in [b"a\x1bb".as_slice(), b"a\0b", b"a\xffb", b"[a](x&#x1b;)", b"&#0000027;", b"&#155;", b"&#x07;"] {
            fs::write(root.join("bad.md"), bad).unwrap();
            let entry = markdown_entry(root, id, Path::new("bad.md")).unwrap();
            assert_eq!(check_text(&entry), Err(NOT_TEXT), "{bad:?}");
        }
        fs::write(root.join("bad.md"), "normal\ttext\r\n&amp; &#169; and &#x26;").unwrap();
        let entry = markdown_entry(root, id, Path::new("bad.md")).unwrap();
        assert_eq!(check_text(&entry), Ok(()));
        fs::OpenOptions::new().write(true).open(root.join("bad.md")).unwrap().set_len(MAX_BYTES + 1).unwrap();
        assert_eq!(check_text(&entry), Err(TOO_LARGE));
        for bytes in [b"bad\npath.md".as_slice(), b"bad\xffpath.md", b"bad\x1bpath.md"] {
            let path = Path::new(OsStr::from_bytes(bytes));
            assert!(matches!(
                markdown_entry(root, id, path),
                Err("Preview needs a UTF-8 path without terminal controls")
            ));
        }
    }

    #[test]
    fn tool_lookup_uses_safe_absolute_path_entries_and_never_runs_the_tool() {
        let fixture = Fixture::folder();
        let root = &fixture.root;
        let outside = root.parent().unwrap();
        let cwd = outside.join("other-cwd");
        let safe = outside.join("user bin '終");
        let safe_link = outside.join("user-bin-link");
        let bad = outside.join("writable-bin");
        for dir in [root, &cwd, &safe, &bad] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("tkmp"), "#!/bin/sh\nprintf invoked > \"$0.invoked\"\n").unwrap();
            fs::set_permissions(dir.join("tkmp"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o777)).unwrap();
        let installed = outside.join("installed-version");
        fs::rename(safe.join("tkmp"), &installed).unwrap();
        symlink("../installed-version", safe.join("tkmp")).unwrap();
        symlink(&safe, &safe_link).unwrap();

        let root_alias = outside.join("project-alias");
        let cwd_alias = outside.join("cwd-alias");
        let outward_alias = outside.join("through-project");
        let tool_alias = outside.join("tool-alias");
        symlink(root, &root_alias).unwrap();
        symlink(&cwd, &cwd_alias).unwrap();
        symlink(&safe, root.join("outward")).unwrap();
        symlink(root.join("outward"), &outward_alias).unwrap();
        fs::create_dir(&tool_alias).unwrap();
        symlink(root.join("tkmp"), tool_alias.join("tkmp")).unwrap();
        let unsafe_dirs =
            [Path::new(""), Path::new("bin"), root, &root_alias, &cwd, &cwd_alias, &bad, &outward_alias, &tool_alias];
        let path = std::env::join_paths(unsafe_dirs).unwrap();
        assert_eq!(installed_on(&path, Some(root), &cwd), None);
        let path = std::env::join_paths(unsafe_dirs.into_iter().chain([safe_link.as_path()])).unwrap();
        assert_eq!(installed_on(&path, Some(root), &cwd), Some(installed.clone()));
        assert!(!installed.with_extension("invoked").exists());
        assert!(!safe.join("tkmp.invoked").exists());
        for mode in [0o644, 0o775, 0o4755] {
            fs::set_permissions(&installed, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(installed_on(&path, Some(root), &cwd), None);
        }
    }
}
