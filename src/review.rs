//! Shows the files of a handed-over patch one at a time, each with only the patch's edits marked.

use std::fmt::Write as _;
use std::path::Path;

use crate::git::Repo;
use crate::handoff::{PATCH_LIMIT, Patch};
use crate::help::Page;
use crate::keys::Key;
use crate::pane::{After, Pane};
use crate::read::{self, Content};
use crate::reader::{Handed, Reader, handed_lines};
use crate::rex;
use crate::screen::Screen;

/// A reader over each file of one patch in turn. It reads the project and
/// changes nothing, in the project or outside it.
pub struct Review {
    repo: Repo,
    /// The patch as it was when the review opened. The files are read again whenever one is shown.
    text: String,
    paths: Vec<String>,
    from: Option<String>,
    at: usize,
    reader: Reader,
}

/// A name to show for whoever handed the patch over: one short line of plain text.
pub fn name(text: &str) -> Option<&str> {
    let plain = !text.trim().is_empty() && !text.chars().any(char::is_control);
    (plain && text.chars().count() <= 40).then_some(text)
}

/// The patch's text. Its file is read once, like any other the user names, and never kept.
fn patch_text(file: &Path) -> Result<String, String> {
    let shown = file.display();
    match read::file(file, PATCH_LIMIT) {
        Content::Bytes(bytes) => String::from_utf8(bytes).map_err(|_| format!("{shown} is not UTF-8 text")),
        Content::TooLarge => Err(format!("{shown} is too large; a patch can be 2 MB")),
        Content::Gone => Err(format!("there is no patch at {shown}")),
        Content::Link(_) | Content::NotFile => Err(format!("{shown} is not a file")),
        Content::Unreadable => Err(format!("{shown} cannot be read")),
    }
}

fn project(dir: &Path) -> Result<Repo, String> {
    Repo::open(dir).ok_or_else(|| "the current directory cannot be read".to_string())
}

fn reader(repo: &Repo, patch: &Patch<'_>, paths: &[String], at: usize, from: Option<&str>) -> Reader {
    let handed = Handed { from: from.map(str::to_owned), number: at + 1, total: paths.len() };
    Reader::handed_over(repo, patch, &paths[at], handed)
}

impl Review {
    /// Opens the patch in `file` against the project that holds `dir`.
    pub fn open(dir: &Path, file: &Path, from: Option<&str>) -> Result<Self, String> {
        let repo = project(dir)?;
        let text = patch_text(file)?;
        let patch = Patch::parse(&text)?;
        let paths: Vec<String> = patch.paths().iter().map(|path| (*path).to_owned()).collect();
        let reader = reader(&repo, &patch, &paths, 0, from);
        Ok(Self { repo, text, paths, from: from.map(str::to_owned), at: 0, reader })
    }

    fn show(&mut self, at: usize) -> After {
        // The text parsed when the review opened, so it parses now.
        if let Ok(patch) = Patch::parse(&self.text) {
            self.reader = reader(&self.repo, &patch, &self.paths, at, self.from.as_deref());
            self.at = at;
        }
        After::Stay
    }
}

impl Pane for Review {
    fn draw(&mut self, screen: &mut Screen) {
        self.reader.draw(screen);
    }

    fn help(&self) -> Page {
        self.reader.help()
    }

    fn notice(&self) -> Option<&'static str> {
        self.reader.notice()
    }

    fn next_frame(&self) -> Option<std::time::Duration> {
        self.reader.next_frame()
    }

    fn key(&mut self, key: Key) -> After {
        let files = self.paths.len();
        match key {
            Key::Char(']') if files > 1 => self.show((self.at + 1) % files),
            Key::Char('[') if files > 1 => self.show((self.at + files - 1) % files),
            Key::Char('r') => self.show(self.at),
            _ => self.reader.key(key),
        }
    }
}

/// Opens the review in a floating Rex pane beside whoever ran this, and says
/// what it will show. That is the only answer a program with no terminal gets,
/// so it names each file whose edits could not be shown.
pub fn float(dir: &Path, file: &Path, from: Option<&str>) -> Result<String, String> {
    let target = rex::Target::current()
        .ok_or("--float needs a Rex pane; run milo review PATCH without --float in a terminal")?;
    let repo = project(dir)?;
    let text = patch_text(file)?;
    let patch = Patch::parse(&text)?;
    let paths = patch.paths();
    let mut report = String::new();
    let mut shown = 0;
    for path in paths {
        match handed_lines(&repo, &patch, path) {
            Ok(_) => shown += 1,
            Err(why) => {
                let name: String = path.chars().filter(|c| !c.is_control()).collect();
                let _ = writeln!(report, "Not shown: {name}: {why}");
            }
        }
    }
    let file = std::path::absolute(file).map_err(|_| "the current directory cannot be read".to_string())?;
    rex::open_review(&target, &repo.root, &file, from)?;
    let files = if paths.len() == 1 { "file" } else { "files" };
    let total = paths.len();
    Ok(format!(
        "Opened a floating reader with the patch's edits in {shown} of {total} {files}.\n{report}\
         It shows the patch as handed over. Milo checked that the files still hold its lines, not who wrote them."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    fn drawn(review: &mut Review, w: usize, h: usize) -> Vec<String> {
        let mut screen = Screen::sized(w, h);
        review.draw(&mut screen);
        screen.text()
    }

    /// A project with committed work, then uncommitted edits by a person, by
    /// the agent whose patch is returned, and by a second agent, in one file.
    fn mixed_project() -> (Fixture, std::path::PathBuf) {
        let f = Fixture::new();
        let lines = |edit: &dyn Fn(usize) -> String| (1..=12).map(edit).collect::<String>();
        f.write("app.txt", &lines(&|n| format!("line {n}\n")));
        f.write("staged.txt", "one\ntwo\nthree\n");
        f.commit("base");
        // Before the agent started: an unstaged edit, a staged edit and a file Git does not know.
        f.write("staged.txt", "one, staged by a person\ntwo\nthree\n");
        f.git(&["add", "staged.txt"]);
        f.write("scratch.txt", "a person's notes\n");
        f.write(
            "app.txt",
            &lines(&|n| match n {
                1 => "line 1, by a person\n".into(),
                4 => "line 4, by the agent\n".into(),
                8 => "line 8, by another agent\n".into(),
                12 => "line 12, by a person later\n".into(),
                n => format!("line {n}\n"),
            }),
        );
        f.write("staged.txt", "one, staged by a person\ntwo\nthree, by the agent\n");
        f.write("new.txt", "made by the agent\n");
        // What the agent hands over: its own three edits, against the last commit.
        let patch = "\
diff --git a/app.txt b/app.txt
--- a/app.txt
+++ b/app.txt
@@ -1,7 +1,7 @@
 line 1
 line 2
 line 3
-line 4
+line 4, by the agent
 line 5
 line 6
 line 7
diff --git a/new.txt b/new.txt
new file mode 100644
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+made by the agent
diff --git a/staged.txt b/staged.txt
--- a/staged.txt
+++ b/staged.txt
@@ -1,3 +1,3 @@
 one
 two
-three
+three, by the agent
";
        let file = f.root.parent().unwrap().join("handed.patch");
        std::fs::write(&file, patch).unwrap();
        (f, file)
    }

    #[test]
    fn only_the_handed_over_edits_are_marked_among_other_uncommitted_work() {
        let (f, patch) = mixed_project();
        let mut review = Review::open(&f.root, &patch, Some("the agent")).unwrap();
        let rows = drawn(&mut review, 70, 16);
        assert!(rows[0].contains("app.txt") && rows[0].ends_with("1 of 3 · patch from the agent"), "{rows:?}");
        let marked: Vec<_> = rows[1..15].iter().filter(|row| row.contains(" + ") || row.contains(" - ")).collect();
        assert_eq!(marked, ["    - line 4", "  4 + line 4, by the agent"], "{rows:?}");
        // The person's and the other agent's lines are in the file, unmarked.
        let plain = |text: &str| rows.iter().any(|row| row.ends_with(text) && !row.contains('+'));
        assert!(plain("line 1, by a person") && plain("line 8, by another agent"), "{rows:?}");

        review.key(Key::Char(']'));
        let rows = drawn(&mut review, 70, 6);
        assert!(rows[0].contains("new.txt") && rows[0].contains("2 of 3"), "{rows:?}");
        assert_eq!(rows[1], " 1 + made by the agent");

        review.key(Key::Char(']'));
        let rows = drawn(&mut review, 70, 6);
        assert_eq!(&rows[1..5], [" 1   one, staged by a person", " 2   two", "   - three", " 3 + three, by the agent"]);
        // A file of the project that the patch does not name is never shown.
        review.key(Key::Char(']'));
        assert!(drawn(&mut review, 70, 6)[0].contains("app.txt"));
    }

    #[test]
    fn a_narrow_review_still_says_which_file_it_shows_and_how_to_reach_the_others() {
        let (f, patch) = mixed_project();
        let mut review = Review::open(&f.root, &patch, Some("omp")).unwrap();
        let last = |review: &mut Review, w: usize| drawn(review, w, 8).pop().unwrap();
        // With the place in the heading, the hint names the keys alone.
        let rows = drawn(&mut review, 80, 8);
        assert!(rows[0].ends_with("1 of 3 · patch from omp"), "{rows:?}");
        assert_eq!(rows[7], " [ ] files · ↑↓ scroll · q close · c changes only · n N changes · ? help");
        // A heading with no room for the place leaves it to the hint, which gives up other keys first.
        let rows = drawn(&mut review, 28, 8);
        assert!(rows[0].ends_with("patch from omp") && !rows[0].contains(" of "), "{rows:?}");
        assert_eq!(rows[7], " [ ] file 1 of 3 · ? help");
        assert_eq!(last(&mut review, 20), " [ ] 1/3 · ? help");
        review.key(Key::Char(']'));
        assert_eq!(last(&mut review, 28), " [ ] file 2 of 3 · ? help");
        // A file with no diff to move through keeps the same place and keys.
        let now = std::fs::read_to_string(f.root.join("staged.txt")).unwrap();
        f.write("staged.txt", &now.replace("three, by the agent", "three, rewritten"));
        review.key(Key::Char(']'));
        assert_eq!(last(&mut review, 28), " [ ] file 3 of 3 · ? help");
        assert_eq!(last(&mut review, 70), " [ ] files · q close · ? help");
        // A patch of one file has nowhere else to go.
        let single = f.root.parent().unwrap().join("single.patch");
        std::fs::write(&single, "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+made by the agent\n").unwrap();
        let mut review = Review::open(&f.root, &single, Some("omp")).unwrap();
        assert_eq!(last(&mut review, 28), " q close · ? help");
    }

    #[test]
    fn a_file_whose_handed_over_lines_were_rewritten_says_so_and_shows_no_diff() {
        let (f, patch) = mixed_project();
        let now = std::fs::read_to_string(f.root.join("app.txt")).unwrap();
        f.write("app.txt", &now.replace("line 4, by the agent", "line 4, rewritten by a person"));
        let mut review = Review::open(&f.root, &patch, None).unwrap();
        let rows = drawn(&mut review, 70, 8);
        assert!(rows[0].ends_with("1 of 3 · handed-over patch"), "{rows:?}");
        let text = rows[1..].join(" ");
        assert!(text.contains("no longer holds the lines the patch left"), "{rows:?}");
        assert!(!text.contains("line 1, by a person") && !text.contains("rewritten"), "{rows:?}");

        // Reloading takes in a file that fits again.
        f.write("app.txt", &now);
        review.key(Key::Char('r'));
        assert!(drawn(&mut review, 70, 16).iter().any(|row| row.ends_with("+ line 4, by the agent")));
    }

    #[test]
    fn the_review_is_read_only_and_leaves_the_project_as_it_was() {
        let (f, patch) = mixed_project();
        let before = std::fs::read(f.root.join("app.txt")).unwrap();
        let mut review = Review::open(&f.root, &patch, None).unwrap();
        drawn(&mut review, 70, 16);
        review.key(Key::Char('e'));
        let rows = drawn(&mut review, 70, 16);
        assert_eq!(rows[15], " Read-only: this shows a patch · ? help");
        assert_eq!(std::fs::read(f.root.join("app.txt")).unwrap(), before);
        review.help().assert_rows_fit(26);
    }

    #[test]
    fn a_patch_cannot_show_a_file_outside_the_project_or_through_a_link() {
        let f = Fixture::folder();
        let outside = f.root.parent().unwrap().join("private.txt");
        std::fs::write(&outside, "secret\n").unwrap();
        std::os::unix::fs::symlink(&outside, f.root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(f.root.parent().unwrap(), f.root.join("up")).unwrap();
        let hunk = "@@ -0,0 +1 @@\n+secret\n";
        let patch = format!(
            "--- a/../private.txt\n+++ b/../private.txt\n{hunk}--- a/link.txt\n+++ b/link.txt\n{hunk}--- a/up/private.txt\n+++ b/up/private.txt\n{hunk}--- {0}\n+++ {0}\n{hunk}",
            outside.display()
        );
        let file = f.root.parent().unwrap().join("outside.patch");
        std::fs::write(&file, patch).unwrap();
        let mut review = Review::open(&f.root, &file, None).unwrap();
        for _ in 0..4 {
            let rows = drawn(&mut review, 90, 6);
            assert!(!rows.iter().any(|row| row.contains("secret")), "{rows:?}");
            assert!(rows[1..].iter().any(|row| row.contains("not a file inside") || row.contains("link")), "{rows:?}");
            review.key(Key::Char(']'));
        }
    }

    #[test]
    fn a_missing_empty_or_shapeless_patch_opens_nothing() {
        let f = Fixture::folder();
        let file = f.root.parent().unwrap().join("none.patch");
        let refused = |text: Option<&str>| {
            match text {
                Some(text) => std::fs::write(&file, text).unwrap(),
                None => drop(std::fs::remove_file(&file)),
            }
            Review::open(&f.root, &file, None).err().unwrap()
        };
        assert!(refused(None).starts_with("there is no patch at "));
        assert!(refused(Some("")).contains("the patch is empty"));
        assert!(refused(Some("M app.txt\nA new.txt\n")).contains("names no file"));
    }

    #[test]
    fn a_name_for_the_patch_is_one_short_plain_line() {
        assert_eq!(name("Claude Code"), Some("Claude Code"));
        assert!(name("").is_none() && name(" ").is_none() && name("a\u{1b}[31m").is_none());
        assert!(name(&"n".repeat(41)).is_none());
    }
}
