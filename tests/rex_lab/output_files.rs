//! Open File from Terminal Output, run in real shell panes of the disposable server.
//!
//! The action is this checkout's `src/setup/actions.lua` under the declarations a save writes
//! above it, run with `rex do FILE --action`, which is how Rex tries an action that is not
//! installed. The stand-in app shows no palette, so where the action would ask, the test
//! passes the choice.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Output;

use super::{Lab, soon};

/// What the panes print. A script prints one of these by name, so that the
/// command typed to print it names no file itself.
const OUTPUTS: [(&str, &str); 5] = [
    ("compiler", "error[E0308]: mismatched types\n   --> src/disk.rs:184:17\n    |\n"),
    ("agent", "⏺ I updated docs/migration.md with the new steps.\n"),
    (
        "several",
        "src/disk.rs:10:4: warning: unused variable\nsrc/disk.rs:20:1: error: expected item\n\
         ⏺ The fix is in src/disk.rs and docs/migration.md.\n",
    ),
    ("none", "Finished v1.2.3 in 0.5s, see https://example.com/src/disk.rs and missing/file.rs:3\n"),
    ("outside", "../outside.txt:1: note\n/etc/hosts\n../second/only-in-second.txt:1\n"),
];

#[test]
#[ignore = "needs an installed Rex; starts a disposable server"]
fn files_named_in_a_panes_output_open_in_a_reader_that_keeps_its_drafts() {
    let mut lab = Lab::start();
    lab.start_app();
    let [first, second] = lab.worktrees();
    let disk: String = (1..=300).map(|n| format!("// line {n} of disk.rs\n")).collect();
    fs::create_dir_all(first.join("src")).unwrap();
    fs::create_dir_all(first.join("docs")).unwrap();
    fs::write(first.join("src/disk.rs"), &disk).unwrap();
    fs::write(first.join("docs/migration.md"), "# Migration\n\nSteps.\n").unwrap();
    lab.git(&first, &["add", "-A"]);
    lab.git(&first, &["commit", "-q", "-m", "Files"]);
    fs::write(lab.root.join("outside.txt"), "not the project's\n").unwrap();
    let outputs = lab.root.join("outputs");
    fs::create_dir(&outputs).unwrap();
    for (name, text) in OUTPUTS {
        fs::write(outputs.join(name), text).unwrap();
    }
    let print = lab.root.join("bin/lab-print");
    fs::write(&print, format!("#!/bin/sh\ncat '{}'/\"$1\"\necho printed-\"$1\"\n", outputs.display())).unwrap();
    fs::set_permissions(&print, fs::Permissions::from_mode(0o700)).unwrap();

    // What `milo setup` saves as `rex.lua`: the layout's declarations, then the actions.
    let actions = lab.root.join("home/.config/milo/rex.lua");
    fs::create_dir_all(actions.parent().unwrap()).unwrap();
    let declarations = format!(
        "local column_width, outline_share, usage_rows, arrange_new_tabs = 0.2, 0.7, 2, false\n\
         local chosen = {{ outline = true }}\nlocal milo = {:?}\n",
        lab.milo.to_str().unwrap()
    );
    let template = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/setup/actions.lua"));
    fs::write(&actions, declarations + template).unwrap();

    let [session, tab] = lab.new_session("output", "build", &first);
    let build = lab.view().tab(&tab).panes[0].block.clone();
    let pane = |label: &str| {
        let view = lab.view();
        view.tab(&tab).panes.iter().find(|pane| pane.label == label).map(|pane| pane.block.clone())
    };
    let panes = || lab.view().tab(&tab).panes.len();
    let screen = |block: &str| lab.rex(&["capture", "-s", &session, "-b", block, "--trim"]);
    let shows = |block: &str, text: &str| {
        assert!(soon(|| screen(block).contains(text)), "expected {text:?} on:\n{}", screen(block));
    };
    let keys = |block: &str, keys: &[&str]| {
        lab.rex(&[["send-key", "-s", session.as_str(), "-b", block].as_slice(), keys].concat());
    };
    // Leaves only this output in the pane, as a program that had just run would.
    let prints = |block: &str, name: &str| {
        lab.rex(&["block", "call", "-s", &session, "-b", block, "clear"]);
        lab.rex(&["send", "-s", &session, "-b", block, &format!("lab-print {name}")]);
        keys(block, &["enter"]);
        shows(block, &format!("\nprinted-{name}"));
    };
    let act = |block: &str, choice: Option<&str>| -> Output {
        let choice = choice.map(|place| format!("choice={place}"));
        let file = actions.to_str().unwrap();
        let mut args = vec!["do", file, "--action", "milo_open_output", "-s", session.as_str(), "-b", block];
        args.extend(choice.as_deref());
        lab.rex_output(&args).unwrap()
    };
    let opens = |block: &str, choice: Option<&str>| {
        let done = act(block, choice);
        assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    };
    let refuses = |block: &str, choice: Option<&str>, why: &str| {
        let before = (panes(), screen(block));
        let done = act(block, choice);
        let said = String::from_utf8_lossy(&done.stderr);
        assert!(!done.status.success() && said.contains(why), "expected {why:?}, got {said:?}");
        assert_eq!((panes(), screen(block)), before, "a refusal changed the tab");
    };

    // One compiler location opens at once, at its line, and the terminal is as it was.
    prints(&build, "compiler");
    let before = screen(&build);
    opens(&build, None);
    assert!(soon(|| pane("disk.rs").is_some()), "no reader in {:?}", lab.view());
    let reader = pane("disk.rs").unwrap();
    let with_reader = panes();
    shows(&reader, " 184   // line 184 of disk.rs");
    assert!(screen(&reader).lines().nth(1).is_some_and(|top| top.contains("// line 181 of")), "{}", screen(&reader));
    assert_eq!(screen(&build), before, "the source terminal changed");

    // Editing starts on that line. The draft stays in the reader through everything below.
    keys(&reader, &["e"]);
    shows(&reader, "^S save");
    lab.rex(&["send", "-s", &session, "-b", &reader, "typed "]);
    shows(&reader, "unsaved");
    shows(&reader, "// line 184 of dtyped isk.rs");

    // A file an agent mentions without a line is a tab of the same reader, beside the draft.
    prints(&build, "agent");
    opens(&build, None);
    shows(&reader, "*disk.rs  migration.md ×");
    shows(&reader, "Steps.");
    assert_eq!(panes(), with_reader);

    // Several places come with what was said about each, most recent first. Asking needs an
    // app that can show the palette; without one the action says so and opens nothing.
    prints(&build, "several");
    let listed = lab
        .command(&lab.milo)
        .args(["output-files", "list", first.to_str().unwrap()])
        .stdin(fs::File::open(outputs.join("several")).unwrap())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        "found 4 outside 0\n\
         src/disk.rs\t⏺ The fix is in src/disk.rs and docs/migration.md.\n\
         docs/migration.md\t⏺ The fix is in src/disk.rs and docs/migration.md.\n\
         src/disk.rs:20:1\tsrc/disk.rs:20:1: error: expected item\n\
         src/disk.rs:10:4\tsrc/disk.rs:10:4: warning: unused variable\n"
    );
    refuses(&build, None, "Choose a file in a connected Rex app, or pass choice.");
    refuses(&build, Some("/etc/hosts"), "That file is not in this pane's recent output.");

    // The pane the action is run in is the one read, whichever pane has the focus. Another
    // worktree's file gets a reader of its own, and the first reader keeps its tabs.
    let made = lab.rex(&["split", "-s", &session, "-b", &build, "-c", second.to_str().unwrap(), "--json"]);
    let other = super::id(&made, "block_ids");
    fs::write(lab.root.join("outputs/second"), "only-in-second.txt:1: note: here\n").unwrap();
    prints(&other, "second");
    lab.rex(&["focus", "-s", &session, &other]);
    opens(&build, Some("src/disk.rs:20:1"));
    // Reopening the tab that has the draft moves its cursor there and keeps the draft.
    shows(&reader, "*disk.rs ×  migration.md");
    shows(&reader, "  20   // line 20 of disk.rs");
    shows(&reader, "unsaved");
    assert_eq!(panes(), with_reader + 1);
    opens(&other, None);
    assert!(soon(|| pane("only-in-second.txt").is_some()), "{:?}", lab.view());
    assert_eq!(panes(), with_reader + 2);
    shows(&pane("only-in-second.txt").unwrap(), "here");
    assert!(!screen(&reader).contains("only-in-second"), "{}", screen(&reader));

    // Output that names no file of the worktree opens nothing and says which it was.
    prints(&build, "none");
    refuses(&build, None, "No project files found in this pane's recent output.");
    prints(&build, "outside");
    refuses(&build, None, "names only files outside its worktree");
    refuses(&build, Some("../outside.txt:1"), "names only files outside its worktree");

    // Nothing but the draft's own save writes the file.
    assert_eq!(fs::read_to_string(first.join("src/disk.rs")).unwrap(), disk);
    keys(&reader, &["ctrl+s"]);
    assert!(soon(|| fs::read_to_string(first.join("src/disk.rs")).unwrap().contains("// line 184 of dtyped isk.rs\n")));
}
