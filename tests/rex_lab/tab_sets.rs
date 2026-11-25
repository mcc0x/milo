//! Reader tab lists in real Rex panes: last tabs, named sets in two
//! worktrees, a draft kept through a reopen, and tabs that are gone.
//!
//! Each reader is started in a pane of its own with `milo view`, and gets
//! its other tabs by reopening a list, so the test asks nothing of the pipe
//! an outline uses. The two palette actions run from this checkout's
//! `actions.lua`, loaded with `rex do FILE --action`.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Lab, soon};

const ACTIONS: &str = include_str!("../../src/setup/actions.lua");

/// Keeps a pane's frame for a pull request, where a folder is named for it.
fn evidence(name: &str, html: &str) {
    if let Some(dir) = std::env::var_os("MILO_TEST_READER_EVIDENCE").map(PathBuf::from) {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}.html")), html).unwrap();
    }
}

/// One Rex tab with a shell, in which readers are opened one at a time.
struct Desk<'a> {
    lab: &'a Lab,
    session: String,
    tab: String,
    shell: String,
    dir: PathBuf,
}

impl<'a> Desk<'a> {
    fn new(lab: &'a Lab, name: &str, dir: &Path) -> Self {
        let [session, tab] = lab.new_session(name, "files", dir);
        let shell = lab.view().tab(&tab).panes[0].block.clone();
        // A pane below keeps a reader beside the shell a readable height.
        let spare = ["--split=below", "--ratio", "56", "--shell=none", "--label", "spare", "--", "/bin/sleep", "600"];
        lab.rex(&[["split", "-s", session.as_str(), "-b", shell.as_str()].as_slice(), &spare].concat());
        Self { lab, session, tab, shell, dir: dir.to_owned() }
    }

    fn labelled(&self, label: &str) -> Option<String> {
        let view = self.lab.view();
        view.tab(&self.tab).panes.iter().find(|pane| pane.label == label).map(|pane| pane.block.clone())
    }

    /// Starts `milo ARGS` in a pane beside the shell and returns its block.
    fn milo(&self, label: &str, args: &[&str]) -> String {
        self.milo_sized(label, "52", args)
    }

    /// The same, in a pane that takes `share` percent of the shell's width.
    fn milo_sized(&self, label: &str, share: &str, args: &[&str]) -> String {
        let place = ["split", "-s", &self.session, "-b", &self.shell, "-c", self.dir.to_str().unwrap()];
        let pane = ["--split=left", "--ratio", share, "--shell=none", "--label", label, "--"];
        let milo = [self.lab.milo.to_str().unwrap()];
        self.lab.rex(&[place.as_slice(), &pane, &milo, args].concat());
        self.pane(label)
    }

    fn pane(&self, label: &str) -> String {
        assert!(soon(|| self.labelled(label).is_some()), "no pane {label:?} in {:?}", self.lab.view());
        self.labelled(label).unwrap()
    }

    fn screen(&self, block: &str) -> String {
        self.lab.rex(&["capture", "-s", &self.session, "-b", block, "--trim"])
    }

    fn shows(&self, block: &str, text: &str) {
        assert!(soon(|| self.screen(block).contains(text)), "expected {text:?} on:\n{}", self.screen(block));
    }

    fn lacks(&self, block: &str, text: &str) {
        assert!(soon(|| !self.screen(block).contains(text)), "unexpected {text:?} on:\n{}", self.screen(block));
    }

    fn keys(&self, block: &str, keys: &[&str]) {
        self.lab.rex(&[["send-key", "-s", self.session.as_str(), "-b", block].as_slice(), keys].concat());
    }

    fn types(&self, block: &str, text: &str) {
        self.lab.rex(&["send", "-s", &self.session, "-b", block, text]);
    }

    /// Presses Esc and waits for `text` to leave the screen, so the next key is not read as Alt with it.
    fn back(&self, block: &str, text: &str) {
        self.keys(block, &["escape"]);
        self.lacks(block, text);
    }

    /// Saves the reader's tabs as a new set through the screen of lists, and leaves that screen.
    fn save_as(&self, block: &str, name: &str) {
        self.keys(block, &["ctrl+t"]);
        self.shows(block, "Save these tabs as a named set…");
        self.keys(block, &["s"]);
        self.shows(block, "Name:");
        self.types(block, name);
        self.shows(block, &format!("Name: {name}"));
        self.keys(block, &["enter"]);
    }

    fn gone(&self, label: &str) {
        assert!(soon(|| self.labelled(label).is_none()), "{label:?} is still in {:?}", self.lab.view());
    }

    fn keep(&self, name: &str, block: &str) {
        evidence(name, &self.lab.rex(&["capture", "-s", &self.session, "-b", block, "--format=html", "--trim"]));
    }

    /// Runs one of the two palette actions from the shell, and returns the floating pane it opens.
    fn action(&self, script: &Path, name: &str) -> String {
        let script = script.to_str().unwrap();
        self.lab.rex(&["do", script, "--action", name, "-s", &self.session, "-b", &self.shell]);
        self.pane("Reader tabs")
    }
}

fn commit(lab: &Lab, dir: &Path, message: &str) -> String {
    lab.git(dir, &["add", "-A"]);
    lab.git(dir, &["commit", "-q", "-m", message]);
    let head = lab.command("git").current_dir(dir).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8(head.stdout).unwrap().trim().to_owned()
}

/// Every list milo keeps, as it is written in the lab's state folder.
fn kept_lists(lab: &Lab) -> String {
    let tabs = lab.root.join("home/.local/state/milo/tabs");
    let mut text = String::new();
    for worktree in fs::read_dir(tabs).unwrap().flatten() {
        for list in fs::read_dir(worktree.path()).unwrap().flatten() {
            text.push_str(&fs::read_to_string(list.path()).unwrap());
        }
    }
    text
}

#[test]
#[ignore = "needs an installed Rex; starts a disposable server"]
fn reader_tabs_reopen_from_last_tabs_and_named_sets_kept_for_each_worktree() {
    let mut lab = Lab::start();
    lab.start_app();
    let [first, second] = ["notes", "notes-review"].map(|name| lab.root.join(name));
    fs::create_dir(&first).unwrap();
    lab.git(&first, &["init", "-q", "--template=", "-b", "main"]);
    fs::write(first.join("a.txt"), "first\n").unwrap();
    fs::write(first.join("b.txt"), "second, as it was\n").unwrap();
    let was = commit(&lab, &first, "Start");
    fs::write(first.join("b.txt"), "second, as it is\n").unwrap();
    commit(&lab, &first, "Reword");
    lab.git(&first, &["worktree", "add", "-q", "-b", "review", second.to_str().unwrap()]);
    fs::write(first.join("c.txt"), "third\n").unwrap();
    let dropped = commit(&lab, &first, "Add c");
    let script = lab.root.join("tab-actions.lua");
    let declared = format!(
        "local milo, column_width, outline_share, usage_rows = \"{}\", 0.2, 0.7, 2\n\
         local arrange_new_tabs = false\nlocal chosen = {{ outline = true }}\n",
        lab.milo.display()
    );
    fs::write(&script, declared + ACTIONS).unwrap();
    let desk = Desk::new(&lab, "notes", &first);

    // A reader of one file saves it as a set. Closing its last tab closes it.
    let reader = desk.milo("one", &["view", "a.txt"]);
    desk.shows(&reader, "first");
    desk.save_as(&reader, "Rex compatibility");
    desk.shows(&reader, "Saved 1 tab as “Rex compatibility”.");
    desk.back(&reader, "Reader tabs");
    desk.keys(&reader, &["q"]);
    desk.gone("one");

    // A reader of b.txt as it was reopens that set beside it, and replaces the set only when told to.
    let reader = desk.milo("two", &["view", "b.txt", "--commit", &was]);
    desk.shows(&reader, "second, as it was");
    desk.keys(&reader, &["ctrl+t"]);
    desk.shows(&reader, "Last tabs");
    desk.keep("lists", &reader);
    desk.keys(&reader, &["down", "down", "enter"]);
    desk.shows(&reader, "Reopen  Rex compatibility");
    desk.shows(&reader, "working file");
    desk.keys(&reader, &["enter"]);
    desk.shows(&reader, &format!("b.txt@{}  a.txt ×", &was[..7]));
    desk.save_as(&reader, "Rex compatibility");
    desk.shows(&reader, "Replace “Rex compatibility” with the 2 open tabs?");
    desk.keep("replace-question", &reader);
    desk.keys(&reader, &["n"]);
    desk.shows(&reader, "1 tab");
    desk.keys(&reader, &["down", "down", "o"]);
    desk.shows(&reader, "Replace “Rex compatibility” with the 2 open tabs?");
    desk.keys(&reader, &["y"]);
    desk.shows(&reader, "Saved 2 tabs as “Rex compatibility”.");
    desk.back(&reader, "Reader tabs");
    desk.keys(&reader, &["ctrl+c"]);
    desk.gone("two");

    // The working b.txt and b.txt at a commit are two tabs. They and a.txt are a second set.
    let reader = desk.milo("three", &["view", "b.txt"]);
    desk.shows(&reader, "second, as it is");
    desk.keys(&reader, &["ctrl+t", "down", "enter"]);
    desk.shows(&reader, "Reopen  Last tabs");
    desk.shows(&reader, &format!("commit {}", &was[..12]));
    desk.keys(&reader, &["enter"]);
    desk.shows(&reader, &format!("b.txt  b.txt@{}  a.txt ×", &was[..7]));
    desk.save_as(&reader, "Parser notes");
    desk.shows(&reader, "Saved 3 tabs as “Parser notes”.");
    desk.back(&reader, "Reader tabs");

    // One tab is closed and another edited. Reopening the set keeps the draft and adds only the closed tab.
    desk.keys(&reader, &["ctrl+w"]);
    desk.lacks(&reader, "a.txt");
    desk.keys(&reader, &["ctrl+p", "e"]);
    desk.shows(&reader, "^S save");
    desk.types(&reader, "draft ");
    desk.shows(&reader, &format!("*b.txt ×  b.txt@{}", &was[..7]));
    desk.keys(&reader, &["ctrl+t", "down", "down", "enter"]);
    desk.shows(&reader, "Reopen  Parser notes");
    desk.shows(&reader, "open · working file");
    desk.shows(&reader, "3 of 3 selected");
    desk.keep("preview-over-draft", &reader);
    desk.keys(&reader, &["enter"]);
    desk.shows(&reader, &format!("*b.txt  b.txt@{}  a.txt ×", &was[..7]));
    desk.keys(&reader, &["ctrl+n"]);
    desk.shows(&reader, "draft second, as it is");
    desk.shows(&reader, "unsaved");
    desk.keep("draft-kept", &reader);
    assert_eq!(fs::read_to_string(first.join("b.txt")).unwrap(), "second, as it is\n");

    // One tab closed is forgotten; the reader closed by Rex leaves the other two as the last tabs.
    desk.keys(&reader, &["ctrl+n", "ctrl+w"]);
    desk.shows(&reader, "*b.txt  a.txt ×");
    lab.rex(&["block", "close", "-s", &desk.session, &reader]);
    desk.gone("three");
    let lists = desk.action(&script, "milo_reopen_tabs");
    desk.shows(&lists, "Reopen  Last tabs");
    desk.shows(&lists, "2 of 2 selected");
    desk.lacks(&lists, "commit ");
    desk.keep("last-tabs", &lists);
    desk.keys(&lists, &["enter"]);
    desk.shows(&lists, "b.txt  a.txt ×");
    // The draft Rex ended is offered back where it always is: at the next edit of its file.
    desk.keys(&lists, &["ctrl+p", "e"]);
    desk.shows(&lists, "Kept draft found");
    desk.keys(&lists, &["d"]);
    desk.shows(&lists, "^S save");
    desk.back(&lists, "^S save");
    desk.keys(&lists, &["ctrl+c"]);
    desk.gone("Reader tabs");

    // A set takes in a tab at a commit that is then dropped, and its working file is deleted.
    let reader = desk.milo("four", &["view", "c.txt", "--commit", &dropped]);
    desk.shows(&reader, "third");
    desk.keys(&reader, &["ctrl+t", "down", "down", "down", "enter"]);
    desk.shows(&reader, "Reopen  Rex compatibility");
    desk.keys(&reader, &["enter"]);
    desk.shows(&reader, &format!("c.txt@{}  b.txt@{}  a.txt ×", &dropped[..7], &was[..7]));
    desk.keys(&reader, &["ctrl+t", "down", "down", "down", "o"]);
    desk.shows(&reader, "Replace “Rex compatibility” with the 3 open tabs?");
    desk.keys(&reader, &["y"]);
    desk.shows(&reader, "Saved 3 tabs as “Rex compatibility”.");
    desk.back(&reader, "Reader tabs");
    desk.keys(&reader, &["ctrl+c"]);
    desk.gone("four");
    lab.git(&first, &["reset", "-q", "--hard", "HEAD~1"]);
    lab.git(&first, &["reflog", "expire", "--expire=now", "--all"]);
    lab.git(&first, &["gc", "-q", "--prune=now"]);
    fs::remove_file(first.join("a.txt")).unwrap();

    // The preview marks both and reopens neither. The working copy never stands in for the commit.
    let lists = desk.action(&script, "milo_tab_sets");
    desk.shows(&lists, "Parser notes");
    desk.keys(&lists, &["down", "down", "enter"]);
    desk.shows(&lists, "Reopen  Rex compatibility");
    desk.shows(&lists, &format!("commit {} unavailable", &dropped[..12]));
    desk.shows(&lists, "missing file");
    desk.shows(&lists, "1 of 3 selected");
    desk.keep("unavailable", &lists);
    desk.keys(&lists, &["space", "down", "space", "down", "space", "enter"]);
    desk.shows(&lists, "No tab is selected.");
    desk.keys(&lists, &["a", "enter"]);
    desk.shows(&lists, "second, as it was");
    desk.lacks(&lists, "×");
    desk.keys(&lists, &["q"]);
    desk.gone("Reader tabs");

    // The other worktree has lists of its own: the same name is a new set there, and asks nothing.
    let other = Desk::new(&lab, "review", &second);
    let reader = other.milo("five", &["view", "a.txt"]);
    other.shows(&reader, "first");
    other.keys(&reader, &["ctrl+t"]);
    other.shows(&reader, "Save these tabs as a named set…");
    other.lacks(&reader, "Rex compatibility");
    other.lacks(&reader, "Last tabs");
    other.back(&reader, "Reader tabs");
    other.save_as(&reader, "Rex compatibility");
    other.shows(&reader, "Saved 1 tab as “Rex compatibility”.");
    other.keep("second-worktree", &reader);

    // Renaming and deleting a set in the first worktree touches that set alone.
    let lists = desk.action(&script, "milo_tab_sets");
    desk.shows(&lists, "Rex compatibility");
    desk.shows(&lists, "3 tabs");
    desk.keys(&lists, &["down", "r"]);
    desk.shows(&lists, "New name: Parser notes");
    desk.keys(&lists, &["ctrl+u"]);
    desk.types(&lists, "Lexer notes");
    desk.keys(&lists, &["enter"]);
    desk.shows(&lists, "Renamed to “Lexer notes”.");
    desk.keys(&lists, &["d"]);
    desk.shows(&lists, "Delete “Lexer notes”? Files and open tabs stay.");
    desk.keep("delete-question", &lists);
    desk.keys(&lists, &["y"]);
    desk.shows(&lists, "Deleted “Lexer notes”.");
    desk.lacks(&lists, "› Lexer notes");
    desk.keys(&lists, &["escape"]);
    desk.gone("Reader tabs");
    other.back(&reader, "Reader tabs");
    other.keys(&reader, &["ctrl+t"]);
    other.shows(&reader, "Rex compatibility");
    other.shows(&reader, "1 tab");

    // A narrow split still names each tab's version and the key that reopens it.
    let narrow = other.milo_sized("narrow", "36", &["tabs"]);
    other.shows(&narrow, "Rex compatibility");
    other.keys(&narrow, &["enter"]);
    other.shows(&narrow, "[x] a.txt");
    other.shows(&narrow, "Enter reopen");
    other.keep("narrow", &narrow);
    other.keys(&narrow, &["enter"]);
    other.shows(&narrow, "first");
    other.keys(&narrow, &["q"]);
    other.gone("narrow");

    // Lists hold paths, commits and lines: no text of a file or a draft, and nothing in either worktree.
    let kept = kept_lists(&lab);
    assert!(kept.contains(&format!("commit {dropped} 0 c.txt")) && kept.contains("working 0 a.txt"), "{kept}");
    assert!(!kept.contains("draft") && !kept.contains("second, as"), "{kept}");
    let status = |dir: &Path| {
        let output = lab.command("git").current_dir(dir).args(["status", "--porcelain"]).output().unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!((status(&first).as_str(), status(&second).as_str()), (" D a.txt\n", ""));
}
