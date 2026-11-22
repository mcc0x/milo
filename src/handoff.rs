//! Works out what a handed-over patch changed in the files as they are now.
//!
//! A *handed-over* patch is a unified diff that a program, usually a coding
//! agent, gave Milo to say which edits were its own. Milo cannot check who
//! wrote a line. It checks only that each file still holds the lines the patch
//! says it left there, and shows those and nothing else as changed. It never
//! asks Git what changed: a file the patch does not fit is shown as
//! unavailable, not filled in from the project's other uncommitted work.

mod fit;
mod patch;

use patch::Section;

/// The most patch text read, the same as the largest file the reader shows.
pub const PATCH_LIMIT: u64 = 2 << 20;
/// The most files of one patch that are listed.
const FILES: usize = 200;

const GONE: &str = "The patch edits this file, but it is gone.";
const STILL_HERE: &str = "The patch removes this file, but it is here.";

/// The file a patch names, as it is in the project now.
pub enum Now<'a> {
    Text(&'a str),
    Gone,
}

pub struct Patch<'a> {
    sections: Vec<Section<'a>>,
    /// Each file the patch names, once, in the patch's order.
    paths: Vec<&'a str>,
}

impl<'a> Patch<'a> {
    /// Reads the patch, or says why there is nothing in it to show.
    pub fn parse(text: &'a str) -> Result<Self, &'static str> {
        if text.trim().is_empty() {
            return Err("the patch is empty, so there are no handed-over edits to show");
        }
        if text.contains('\0') {
            return Err("the patch is not text");
        }
        let sections = patch::parse(text);
        if sections.is_empty() {
            return Err("the patch names no file; expected a unified diff, as git diff or diff -u writes");
        }
        let mut paths = Vec::new();
        for section in &sections {
            if !paths.contains(&section.path) {
                if paths.len() == FILES {
                    return Err("the patch names more than 200 files, which is more than a review shows");
                }
                paths.push(section.path);
            }
        }
        Ok(Self { sections, paths })
    }

    pub fn paths(&self) -> &[&'a str] {
        &self.paths
    }

    /// Why the patch itself rules out showing this file, known without looking for the file.
    pub fn problem(&self, path: &str) -> Option<&'static str> {
        self.of(path).find_map(|section| section.problem)
    }

    fn of<'s>(&'s self, path: &'s str) -> impl DoubleEndedIterator<Item = &'s Section<'a>> {
        self.sections.iter().filter(move |section| section.path == path)
    }

    /// The whole file as a diff in which only the patch's edits are changes,
    /// or why they cannot be told apart from the rest of the file.
    pub fn whole_file_diff(&self, path: &str, now: Now<'_>) -> Result<Vec<u8>, &'static str> {
        if let Some(problem) = self.problem(path) {
            return Err(problem);
        }
        let removed = self.of(path).next_back().is_some_and(|section| section.removes_file);
        let text = match (now, removed) {
            (Now::Text(_), true) => return Err(STILL_HERE),
            (Now::Gone, false) => return Err(GONE),
            (Now::Text(text), false) => text,
            (Now::Gone, true) => "",
        };
        fit::whole_file_diff(text, self.of(path).rev())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_FILES: &str = "--- a/one.txt\n+++ b/one.txt\n@@ -1 +1 @@\n-a\n+A\n--- a/two.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-b\n--- a/one.txt\n+++ b/one.txt\n@@ -1 +1,2 @@\n A\n+more\n";

    #[test]
    fn a_patch_lists_each_file_once_and_joins_its_edits() {
        let patch = Patch::parse(TWO_FILES).unwrap();
        assert_eq!(patch.paths(), ["one.txt", "two.txt"]);
        let diff = patch.whole_file_diff("one.txt", Now::Text("A\nmore\n")).unwrap();
        assert!(diff.ends_with(b"\n-a\n+A\n+more\n"), "{}", String::from_utf8_lossy(&diff));
    }

    #[test]
    fn a_file_that_is_gone_or_back_is_unavailable_unless_the_patch_says_so() {
        let patch = Patch::parse(TWO_FILES).unwrap();
        assert_eq!(patch.whole_file_diff("one.txt", Now::Gone), Err(GONE));
        assert_eq!(patch.whole_file_diff("two.txt", Now::Text("b\n")), Err(STILL_HERE));
        assert!(patch.whole_file_diff("two.txt", Now::Gone).unwrap().ends_with(b"\n-b\n"));
    }

    #[test]
    fn one_unreadable_edit_makes_the_whole_file_unavailable() {
        let text = format!("{TWO_FILES}--- a/one.txt\n+++ b/one.txt\n@@ -1,9 +1,9 @@\n cut short\n");
        let patch = Patch::parse(&text).unwrap();
        assert_eq!(patch.whole_file_diff("one.txt", Now::Text("A\nmore\n")), Err(patch::MALFORMED));
    }

    #[test]
    fn an_empty_patch_or_plain_prose_is_refused_with_the_reason() {
        assert!(Patch::parse(" \n").is_err_and(|why| why.contains("empty")));
        assert!(Patch::parse("I edited src/a.rs and src/b.rs.\n").is_err_and(|why| why.contains("unified diff")));
    }

    #[test]
    fn a_patch_with_a_nul_or_too_many_files_is_refused_whole() {
        assert!(
            Patch::parse("--- a/f\n+++ b/f\n@@ -1,2 +1 @@\n-x\0y\n keep\n").is_err_and(|why| why.contains("not text"))
        );
        let files = |count: usize| -> String {
            (0..count).map(|n| format!("--- a/f{n}\n+++ b/f{n}\n@@ -1 +1 @@\n-a\n+b\n")).collect()
        };
        assert_eq!(Patch::parse(&files(FILES)).unwrap().paths().len(), FILES);
        assert!(Patch::parse(&files(FILES + 1)).is_err_and(|why| why.contains("more than 200 files")));
    }

    #[test]
    fn names_outside_the_project_are_known_before_any_file_is_looked_for() {
        let patch =
            Patch::parse("--- a/../x\n+++ b/../x\n@@ -1 +1 @@\n-a\n+b\n--- a/ok\n+++ b/ok\n@@ -1 +1 @@\n-a\n+b\n")
                .unwrap();
        assert_eq!(patch.paths(), ["../x", "ok"]);
        assert_eq!((patch.problem("../x"), patch.problem("ok")), (Some(patch::NOT_INSIDE), None));
    }
}
