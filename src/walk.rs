//! Lists a directory tree without git, for folders that are not repositories.

use std::path::Path;

/// Entries listed before the walk stops descending, so opening every folder
/// of something enormous cannot hang the outline.
const CAP: usize = 50_000;

/// Every entry of `root` and of each directory `open` says is expanded, as
/// sorted paths relative to `root`. Directories end with `/`; one that is not
/// open is listed without its contents, which are read when it is opened.
/// Symbolic links are listed, never followed.
pub fn list(root: &Path, open: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut dirs = vec![String::new()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(root.join(&dir)) else { continue };
        for entry in entries.flatten() {
            let mut path = format!("{dir}{}", entry.file_name().to_string_lossy());
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if out.len() < CAP && open(&path) {
                    dirs.push(format!("{path}/"));
                }
                path.push('/');
            }
            out.push(path);
        }
    }
    out.sort_unstable();
    out
}
