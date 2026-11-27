//! Real Rex and Milo panes, with a sizing shim instead of a GUI client.
//! The external renderer is a stand-in that records argv without rendering.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use super::{Lab, soon};

#[test]
#[ignore = "needs an installed Rex; starts a disposable server"]
fn markdown_handoff_reuses_only_its_own_split() {
    let mut lab = Lab::start();
    let config = lab.root.join("home/.config/rex");
    fs::create_dir(&config).unwrap();
    fs::write(config.join("init.lua"), "rex.bind(\"ctrl+a>z\", \"pane.zoom\")\n").unwrap();
    lab.rex(&["config", "reload"]);
    let keymap = lab.rex(&["keymap"]);
    assert!(keymap.lines().any(|line| line.split_whitespace().take(3).eq(["ctrl+a", "z", "pane.zoom"])), "{keymap}");
    // CLI key injection targets the terminal directly. Check the loaded client
    // binding above, then exercise only the keys still available in the pane.
    lab.start_app();
    let dir = lab.root.join("field notes");
    fs::create_dir(&dir).unwrap();
    let first = "- draft '水' $(echo nope);.md";
    let second = "route.md";
    fs::write(dir.join(first), "# Field plan\n\nMeet at the trailhead.\n").unwrap();
    fs::write(dir.join(second), "# Route\n\nFollow the river.\n").unwrap();
    let [session, tab] = lab.new_session("field-notes", "planning", &dir);
    let shell = lab.view().tab(&tab).panes[0].block.clone();
    let label = "milo:markdown-preview";
    // A user-chosen label is not proof Milo owns this long-running command.
    lab.rex(&["split", "-s", &session, "-b", &shell, "--label", label, "--shell=none", "--", "/bin/sleep", "600"]);
    let other = lab.view().tab(&tab).pane(label).block.clone();
    // Give Outline a pane-specific TMPDIR. Rex starts the floating Reader
    // below with the server's environment instead.
    let temp = lab.root.join("login-tmp");
    fs::create_dir(&temp).unwrap();
    let temp = format!("TMPDIR={}", temp.display());
    // Rex can supply an escaped Unix URL, e.g. its default macOS socket has a space.
    let endpoint = format!("REX_SERVER={}", lab.socket().replace("/s.sock", "/%73.sock"));
    lab.rex(&[
        "split",
        "-s",
        &session,
        "-b",
        &shell,
        "--split=left",
        "--shell=none",
        "--label",
        "milo:outline",
        "-c",
        dir.to_str().unwrap(),
        "--",
        "/usr/bin/env",
        &temp,
        &endpoint,
        lab.milo.to_str().unwrap(),
        "outline",
        "--shell",
        &shell,
    ]);
    let outline = lab.view().tab(&tab).pane("milo:outline").block.clone();
    let keys = |block: &str, keys: &[&str]| {
        lab.rex(&[["send-key", "-s", session.as_str(), "-b", block].as_slice(), keys].concat());
    };
    let screen = |block: &str| lab.rex(&["capture", "-s", &session, "-b", block, "--trim"]);
    let wait = |block: &str, predicate: &dyn Fn(&str) -> bool| {
        let start = Instant::now();
        loop {
            let frame = screen(block);
            if predicate(&frame) {
                break frame;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "unexpected terminal frame:\n{frame}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let preview = |block: &str, actions: &str| {
        keys(block, &[actions]);
        let menu = wait(block, &|frame| frame.contains("Preview with tkmp"));
        let selected = menu.lines().position(|line| line.trim_start().starts_with('›')).unwrap();
        let wanted = menu.lines().position(|line| line.contains("Preview with tkmp")).unwrap();
        for _ in selected..wanted {
            keys(block, &["down"]);
        }
        keys(block, &["enter"]);
    };
    wait(&outline, &|frame| frame.contains(first));
    keys(&outline, &["a"]);
    wait(&outline, &|frame| frame.contains("tkmp not installed"));
    keys(&outline, &["end"]);
    wait(&outline, &|frame| {
        frame.lines().any(|line| line.trim_start().starts_with('›') && line.contains("tkmp not installed"))
    });
    keys(&outline, &["enter"]);
    wait(&outline, &|frame| frame.contains("tkmp not installed"));
    keys(&outline, &["escape"]);
    wait(&outline, &|frame| frame.contains(first) && !frame.contains("tkmp not installed"));
    assert_eq!(lab.view().tab(&tab).panes.len(), 3, "a missing tool must not open a split");

    let tool = lab.root.join("bin/tkmp");
    let log = lab.root.join("calls");
    // With no wrapper, the recorder must receive just the literal file path.
    fs::write(&tool, concat!(
        "#!/bin/sh\n",
        "printf '%s\\n' \"$#\" \"$@\" \"${TKMP_LOG-unset}\" \"${TKMP_DUMP-unset}\" \"$PWD\" >> \"${0%/*}/../calls\"\n",
        "exec /bin/sleep 600\n",
    )).unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
    // Detection alone never executes the file, not even to ask for its version.
    let doctor = lab.command(&lab.milo).current_dir(&dir).arg("doctor").output().unwrap();
    assert!(doctor.status.success());
    assert!(String::from_utf8(doctor.stdout).unwrap().contains("tkmp found at"));
    assert!(!log.exists());
    let mut expected = String::new();
    let mut check_call = |file: &str, options: &[&str]| {
        expected.push_str(&format!("{}\n", options.len() + 1));
        for option in options {
            expected.push_str(option);
            expected.push('\n');
        }
        expected.push_str(&format!("{}\nunset\nunset\n/\n", dir.join(file).display()));
        assert!(soon(|| lab.read("calls") == expected), "handoff record: {:?}", lab.read("calls"));
        // The child can start before Rex finishes removing the previous split.
        assert!(soon(|| lab.view().tab(&tab).panes.iter().filter(|p| p.label == label).count() == 2));
        let view = lab.view();
        let panes = &view.tab(&tab).panes;
        let i = panes.iter().position(|p| p.block == shell).unwrap();
        let owned = &panes[i + 1];
        assert_eq!(owned.label, label, "preview should split the shell on its right: {panes:?}");
        assert_ne!(owned.block, other);
        assert!(panes.iter().any(|p| p.block == other));
        owned.block.clone()
    };
    preview(&outline, "a");
    let first_preview = check_call(first, &[]);

    // The user's PATH wrapper owns the width setting; Milo still passes only
    // the filename. Use an absolute recorder path so tkmp cannot call itself.
    let recorder = lab.root.join("bin/record-tkmp");
    fs::rename(&tool, &recorder).unwrap();
    fs::write(&tool, format!("#!/bin/sh\nexec '{}' --max-width 0 \"$@\"\n", recorder.display())).unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
    let width_options = &["--max-width", "0"];

    keys(&outline, &["[Slash]", "a"]);
    wait(&outline, &|frame| {
        frame.lines().next().is_some_and(|line| line.trim_start().starts_with("/ a "))
            && frame.contains(first)
            && !frame.contains(second)
    });
    keys(&outline, &["backspace", "r", "o", "u", "t", "e"]);
    wait(&outline, &|frame| frame.contains("/ route") && frame.contains(second));
    let calls = lab.read("calls");
    fs::write(dir.join(second), b"# Invalid encoding\n\xff").unwrap();
    preview(&outline, "ctrl+t");
    wait(&outline, &|frame| frame.contains("Preview needs UTF-8"));
    assert_eq!(lab.read("calls"), calls, "an unreadable selection must not reach tkmp");
    assert!(lab.view().tab(&tab).panes.iter().any(|p| p.block == first_preview));
    fs::write(dir.join(second), "# Route\n\nFollow the river.\n").unwrap();
    preview(&outline, "ctrl+t");
    assert_ne!(check_call(second, width_options), first_preview);

    // Enter in the tree still opens Milo. A floating Reader targets the same shell
    // as its opener, even though the active block is no longer that shell.
    keys(&outline, &["escape"]);
    wait(&outline, &|frame| frame.contains(first));
    keys(&outline, &["enter"]);
    let reader = || lab.view().tab(&tab).panes.iter().find(|p| p.label == first).map(|p| p.block.clone());
    assert!(soon(|| reader().is_some()), "no floating Reader: {:?}", lab.view());
    let reader = reader().unwrap();
    wait(&reader, &|frame| frame.contains("Meet at the trailhead"));
    preview(&reader, "a");
    let moved = check_call(first, width_options);
    wait(&reader, &|frame| frame.contains("Meet at the trailhead"));

    // A pane the user moved to another tab is no longer ours to replace.
    let elsewhere = lab.new_tab(&session, &dir);
    let anchor = lab.view().tab(&elsewhere).panes[0].block.clone();
    lab.rex(&["move", "-s", &session, &moved, "right", &anchor]);
    preview(&reader, "a");
    assert_ne!(check_call(first, width_options), moved);
    assert!(lab.view().tab(&elsewhere).panes.iter().any(|p| p.block == moved));
}
