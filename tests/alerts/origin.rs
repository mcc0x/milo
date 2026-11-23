//! The focused tab belongs to neither alert. Only the Rex event and its exact session view agree.

use std::fs;
use std::io::Write as _;
use std::os::unix::net::UnixListener;

use super::{Home, soon, text_of};

const HOST: &str = "00000000-0000-4000-8000-000000000001";
const A: &str = "00000000-0000-4000-8000-00000000000a";
const B: &str = "00000000-0000-4000-8000-00000000000b";
const FOCUS: &str = "00000000-0000-4000-8000-00000000000f";

const REX: &str = r#"#!/bin/sh
case " $* " in
  *' whoami --json '*) cat "$HOME/identity";;
  *' session inspect '*)
    for arg do
      case "$arg" in session:*) echo "$arg" >> "$HOME/lookups"; exec cat "$HOME/$arg";; esac
    done
    exit 1;;
  *' events --all --json '*) echo stream >> "$HOME/started"; exec tail -c +1 -f "$HOME/stream";;
  *) exit 1;;
esac
"#;

fn view(home: &Home, id: &str, label: &str) {
    let window = |id: &str, name: &str| {
        format!(
            r#"{{"window_id":"window:{id}","label":"{name}","layers":[{{"blocks":[{{"block_id":"block:{id}","creator_name":"com.superlogical.terminal","host_id":"{HOST}"}}]}}]}}"#
        )
    };
    let windows = [window(FOCUS, "wrong-label"), window(id, "not-the-session-name")].join(",");
    fs::write(home.root.join(format!("session:{id}")), format!(
        r#"{{"session_id":"session:{id}","label":"{label}","active_window_id":"window:{FOCUS}","windows":[{windows}]}}"#
    )).unwrap();
}

fn emit(home: &Home, id: &str, state: &str) {
    let mut file = fs::OpenOptions::new().append(true).open(home.root.join("stream")).unwrap();
    writeln!(file, r#"{{"type":"session_event","payload":{{"session_id":"session:{id}","event":{{"type":"block_event","payload":{{"block_id":"block:{id}","creator_name":"com.superlogical.terminal","name":"program_status_changed","payload":{{"id":"","record":{{"state":"{state}","app":"amp","title":"private prompt","msg":"/private/path"}}}}}}}}}}}}"#).unwrap();
}

fn delivered(home: &Home, index: usize, action: &str, label: Option<&str>, text: bool) {
    for (route, mut expected, code) in [
        ("/bot", format!("Rex: <code>amp</code> {action}"), ("<code>", "</code>")),
        ("/api/v10/webhooks/", format!("`amp` **{action}**"), ("`", "`")),
    ] {
        assert!(soon(|| home.destination.requests_at(route).len() > index), "{}", home.run(&["alerts"]));
        if let Some(label) = label {
            expected.push_str(&format!(" · {}{label}{}", code.0, code.1));
        }
        if text {
            expected.push_str(&format!("\n{}private prompt: /private/path{}", code.0, code.1));
        }
        assert_eq!(text_of(&home.destination.requests_at(route)[index].body), expected);
    }
}

#[test]
fn both_providers_follow_the_triggering_terminal_only_when_chosen() {
    let Some(home) = Home::new() else { return };
    let path = home.root.join("rex.sock");
    let _socket = UnixListener::bind(&path).unwrap();
    let endpoint = path.display();
    fs::write(
        home.root.join("identity"),
        format!(
            r#"{{"host_id":"{HOST}","endpoint":"unix://{endpoint}","principal":{{"kind":"peer","transport":"unix"}}}}"#
        ),
    )
    .unwrap();
    fs::write(home.root.join("bin/rex"), REX).unwrap();
    view(&home, A, "release-review");
    view(&home, B, "build-check");
    home.save_telegram();
    home.save_discord();
    home.run(&["alerts", "content", "text"]);
    home.run(&["alerts", "on", "telegram"]);
    home.run(&["alerts", "on", "discord"]);
    emit(&home, A, "done");
    delivered(&home, 0, "finished", None, true);
    assert!(!home.root.join("lookups").exists());

    home.run(&["alerts", "off"]);
    fs::write(home.root.join("stream"), "").unwrap(); // Rex doesn't replay old events to a new subscriber.
    home.run(&["alerts", "origin", "on"]);
    home.run(&["alerts", "content", "metadata"]);
    home.run(&["alerts", "on"]);
    emit(&home, A, "done");
    emit(&home, B, "done");
    delivered(&home, 1, "finished", Some("release-review"), false);
    delivered(&home, 2, "finished", Some("build-check"), false);
    // A close wins the race with inspection. Neither cached label nor focus is a substitute.
    fs::remove_file(home.root.join(format!("session:{A}"))).unwrap();
    emit(&home, A, "error");
    delivered(&home, 3, "reported an error", None, false);
    // A rename won the lookup race: use the current label of the same session, never another tab.
    view(&home, B, "renamed-session");
    emit(&home, B, "error");
    delivered(&home, 4, "reported an error", Some("renamed-session"), false);

    // Revoke while Telegram has a labeled request waiting to retry. The already-running
    // worker must honor the saved choice even before a settings client can stop it.
    home.destination.answer_at("/bot", &[(429, r#"{"parameters":{"retry_after":5}}"#)]);
    emit(&home, B, "done");
    delivered(&home, 5, "finished", Some("renamed-session"), false);
    let starts = home.starts();
    fs::write(home.root.join("stream"), "").unwrap();
    let conf = home.root.join(".config/milo/alerts.conf");
    let before = fs::read_to_string(&conf).unwrap();
    let after = before.replace("origin = true", "origin = false");
    assert_ne!(before, after);
    let next = conf.with_extension("new");
    fs::write(&next, after).unwrap();
    fs::rename(next, conf).unwrap();
    assert!(soon(|| home.starts() > starts));
    // The old delivery workers have exited; their queued request was not retried.
    assert_eq!(home.destination.requests_at("/bot").len(), 6);
    emit(&home, B, "error");
    let discord = || home.destination.requests_at("/api/v10/webhooks/");
    assert!(soon(|| discord().len() == 7));
    assert_eq!(text_of(&discord()[6].body), "`amp` **reported an error**");
}
