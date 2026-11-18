//! Lists a directory tree without git, for folders that are not repositories.

use std::path::Path;

/// Stop adding folders to the walk after this many entries have been listed.
const CAP: usize = 50_000;

/// Sorted paths relative to `root`. Folders end in `/` and their contents are
/// listed only if `open` says to descend. Symbolic links are not followed.
pub fn list(root: &Path, open: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut dirs = vec![String::new()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(root.join(&dir)) else { continue };
        for entry in entries.flatten() {
            // Hide Finder's folder metadata in the plain tree.
            if entry.file_name() == ".DS_Store" {
                continue;
            }
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
