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
/// Paths with names the outline cannot represent are omitted and reported.
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
            let Some(name) = name.to_str() else {
                // A lossy conversion could turn this into a different entry's path.
                listing.unreadable = true;
                continue;
            };
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
            let is_dir = kind.is_dir();
            let mut path = String::with_capacity(dir.len() + name.len() + usize::from(is_dir));
            path.push_str(&dir);
            path.push_str(name);
            if is_dir {
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn an_expanded_tree_does_not_descend_into_directory_links() {
        let f = Fixture::folder();
        f.write("real/file", "");
        symlink("real", f.root.join("link")).unwrap();
        let listing = list(&f.root, |_| true);
        assert!(!listing.unreadable && !listing.truncated);
        assert_eq!(listing.paths, ["link", "real/", "real/file"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn invalid_utf8_does_not_alias_a_real_folder() {
        use std::ffi::OsStr;
        use std::fs;
        use std::os::unix::ffi::OsStrExt as _;

        let f = Fixture::folder();
        fs::create_dir(f.root.join(OsStr::from_bytes(b"folder\xff"))).unwrap();
        f.write("folder\u{fffd}/file", "");
        let listing = list(&f.root, |_| true);
        assert!(listing.unreadable);
        assert!(!listing.truncated);
        assert_eq!(listing.paths, ["folder\u{fffd}/", "folder\u{fffd}/file"]);
    }
}
