//! Lists a directory tree without git, for folders that are not repositories.

use std::path::Path;

/// Maximum number of entries retained in a plain tree.
pub(super) const CAP: usize = 50_000;

#[derive(Default)]
pub struct Listing {
    pub paths: Vec<String>,
    pub unreadable: bool,
    pub truncated: bool,
}

/// Sorted paths relative to `root`. Folders end in `/` and their contents are
/// listed only if `open` says to descend. Symbolic links are not followed.
/// A truncated listing is sorted too, but doesn't enumerate the rest.
pub fn list(root: &Path, open: impl Fn(&str) -> bool) -> Listing {
    let mut listing = Listing::default();
    let mut dirs = vec![String::new()];
    'walk: while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(root.join(&dir)) else {
            listing.unreadable = true;
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                listing.unreadable = true;
                continue;
            };
            let name = entry.file_name();
            // Hide Finder's folder metadata in the plain tree.
            if name == ".DS_Store" {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                listing.unreadable = true;
                continue;
            };
            if listing.paths.len() == CAP {
                listing.truncated = true;
                break 'walk;
            }
            let mut path = format!("{dir}{}", name.to_string_lossy());
            if kind.is_dir() {
                path.push('/');
                if open(&path) {
                    dirs.push(path.clone());
                }
            }
            listing.paths.push(path);
        }
    }
    listing.paths.sort_unstable();
    listing
}
