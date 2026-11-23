//! Exercise origin attribution and recovery without attaching to a user session.

use std::fs;
use std::io::{ErrorKind, Read as _, Write as _};
use std::os::unix::net::UnixListener;

use super::{DISCORD, ENABLED, Home, PAUSED, RECOVERED, TELEGRAM, settle, soon};

const HOST: &str = "00000000-0000-4000-8000-000000000001";
const A: &str = "00000000-0000-4000-8000-00000000000a";
const B: &str = "00000000-0000-4000-8000-00000000000b";
const FOCUS: &str = "00000000-0000-4000-8000-00000000000f";

fn bind(home: &Home) -> UnixListener {
    let socket = UnixListener::bind(home.rex_socket()).unwrap();
    let endpoint = home.rex_endpoint();
    fs::write(
        home.root.join("identity"),
        format!(r#"{{"host_id":"{HOST}","endpoint":"{endpoint}","principal":{{"kind":"peer","transport":"unix"}}}}"#),
    )
    .unwrap();
    socket
}

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

fn expect_message(home: &Home, route: &str, index: usize, expected: &str) {
    assert!(soon(|| home.destination.requests_at(route).len() > index), "{}", home.run(&["alerts"]));
    assert_eq!(home.destination.texts_at(route)[index], expected);
}

fn delivered(home: &Home, index: usize, action: &str, label: Option<&str>, text: bool) {
    for (route, mut expected, code) in [
        (TELEGRAM, format!("Rex: <code>amp</code> {action}"), ("<code>", "</code>")),
        (DISCORD, format!("`amp` **{action}**"), ("`", "`")),
    ] {
        if let Some(label) = label {
            expected.push_str(&format!(" · {}{label}{}", code.0, code.1));
        }
        if text {
            expected.push_str(&format!("\n{}private prompt: /private/path{}", code.0, code.1));
        }
        expect_message(home, route, index, &expected);
    }
}

#[test]
fn both_providers_follow_the_triggering_terminal_only_when_chosen() {
    let Some(home) = Home::new_unbound() else { return };
    let _socket = bind(&home);
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
    home.checkpoint();
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
    assert!(soon(|| home.run(&["alerts"]).contains("Trying again: Telegram is limiting the rate")));
    let quiet_until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let starts = home.starts();
    home.checkpoint();
    let conf = home.root.join(".config/milo/alerts.conf");
    let before = fs::read_to_string(&conf).unwrap();
    let after = before.replace("origin = true", "origin = false");
    assert_ne!(before, after);
    let next = conf.with_extension("new");
    fs::write(&next, after).unwrap();
    fs::rename(next, conf).unwrap();
    assert!(soon(|| home.starts() > starts));
    // Changing privacy must not cancel Telegram's quiet period. Let the full
    // retry window elapse, then check that the revoked labeled POST never ran.
    std::thread::sleep(quiet_until.saturating_duration_since(std::time::Instant::now()));
    assert_eq!(home.destination.requests_at(TELEGRAM).len(), 6);
    emit(&home, B, "error");
    delivered(&home, 6, "reported an error", None, false);
    assert_eq!(home.destination.requests_at(TELEGRAM).len(), 7);
    assert_eq!(home.destination.requests_at(DISCORD).len(), 7);
}

fn recover(origin: bool) {
    let Some(home) = Home::new_unbound() else { return };
    let socket = bind(&home);
    view(&home, A, "before-restart");
    home.save_telegram();
    home.save_discord();
    if origin {
        home.run(&["alerts", "origin", "on"]);
    }
    for (provider, route) in [("telegram", TELEGRAM), ("discord", DISCORD)] {
        home.run(&["alerts", "notices", "on", provider]);
        home.run(&["alerts", "on", provider]);
        expect_message(&home, route, 0, ENABLED);
    }
    home.wait_for_health("Listening");
    emit(&home, A, "done");
    delivered(&home, 1, "finished", origin.then_some("before-restart"), false);
    home.checkpoint();

    home.rex_fault("rex-down", true);
    home.wait_for_health("Waiting for Rex");
    home.run(&["alerts", "off", "telegram"]);
    expect_message(&home, TELEGRAM, 2, PAUSED);
    // The successor owns a new socket at the same path while the old inode is
    // still held open. Its session context must be resolved afresh.
    fs::remove_file(home.rex_socket()).unwrap();
    let _replacement = bind(&home);
    drop(socket);
    view(&home, B, "after-restart");
    home.rex_fault("rex-down", false);
    home.wait_for_health("Listening");
    expect_message(&home, DISCORD, 2, RECOVERED);
    home.login();

    emit(&home, B, "error");
    let expected =
        if origin { "`amp` **reported an error** · `after-restart`" } else { "`amp` **reported an error**" };
    expect_message(&home, DISCORD, 3, expected);

    // Two matching block IDs in different tabs are not sufficient evidence.
    // Neither the session label, the focused tab nor the program's text is sent.
    let session = home.root.join(format!("session:{B}"));
    let exact_view = fs::read_to_string(&session).unwrap();
    let ambiguous = exact_view.replace(&format!("block:{FOCUS}"), &format!("block:{B}"));
    assert_ne!(exact_view, ambiguous);
    fs::write(&session, ambiguous).unwrap();
    emit(&home, B, "done");
    expect_message(&home, DISCORD, 4, "`amp` **finished**");

    if origin {
        home.checkpoint();
        let supervisor = home.supervisor().expect("listening without a supervisor");
        let killed = home.crash_worker();
        home.wait_for_health("Retrying");
        view(&home, B, "worker-replacement");
        home.wait_for_health("Listening");
        assert_eq!(home.supervisor(), Some(supervisor));
        assert_ne!(home.worker().expect("listening without a worker"), killed);
        // The socket is unchanged, but the new worker must resolve the label
        // again. This crash is inside the same interruption's settling window,
        // so there must be no second recovery notice.
        emit(&home, B, "done");
        expect_message(&home, DISCORD, 5, "`amp` **finished** · `worker-replacement`");
    }
    settle();
    assert_eq!(home.destination.texts_at(TELEGRAM).len(), 3, "paused provider received another message");
    assert_eq!(
        home.destination.texts_at(DISCORD).len(),
        5 + usize::from(origin),
        "active provider received a duplicate message"
    );
    for (route, indices) in [(TELEGRAM, &[0, 2][..]), (DISCORD, &[0, 2, 4][..])] {
        let requests = home.destination.requests_at(route);
        for &index in indices {
            let body = &requests[index].body;
            for private in [
                "before-restart", "after-restart", "worker-replacement", "wrong-label", "not-the-session-name",
                "private prompt", "/private/path", A, B,
            ] {
                assert!(!body.contains(private), "{route} request {index} leaked {private}: {body}");
            }
        }
    }
    let lookups = home.root.join("lookups");
    if origin {
        assert_eq!(
            fs::read_to_string(lookups).unwrap(),
            format!("session:{A}\nsession:{B}\nsession:{B}\nsession:{B}\n")
        );
    } else {
        assert!(!lookups.exists(), "origin was looked up despite the default being off");
    }
}

#[test]
fn recovery_keeps_origin_off_by_default_and_keeps_one_provider_paused() {
    recover(false);
}

#[test]
fn recovery_refreshes_opted_in_origin_without_leaking_it_into_notices_or_ambiguous_alerts() {
    recover(true);
}

#[test]
fn replacing_the_socket_during_subscription_open_does_not_report_false_recovery() {
    let Some(home) = Home::new_unbound() else { return };
    let original = bind(&home);
    view(&home, A, "original-context");
    home.save_discord();
    home.run(&["alerts", "origin", "on"]);
    home.run(&["alerts", "notices", "on", "discord"]);
    home.run(&["alerts", "on", "discord"]);
    home.wait_for_health("Listening");
    expect_message(&home, DISCORD, 0, ENABLED);
    emit(&home, A, "done");
    expect_message(&home, DISCORD, 1, "`amp` **finished** · `original-context`");
    home.checkpoint();

    home.rex_fault("rex-down", true);
    home.wait_for_health("Waiting for Rex");
    let mut lease = home.rex_lease();
    home.rex_fault("rex-down", false);
    // The FIFO byte comes from the new subscriber only after it has passed
    // the fault gate. It stays alive even if the socket pathname changes.
    let mut ready = [0];
    assert!(soon(|| match lease.read(&mut ready) {
        Ok(1) => true,
        Ok(0) => false,
        Err(error) if error.kind() == ErrorKind::WouldBlock => false,
        result => panic!("unexpected Rex lease data: {result:?}"),
    }));
    assert_eq!(ready, [b'x']);
    let unconfirmed = home.starts();
    // Refuse every later subscription until we have observed the stale
    // attempt retire without announcing that Rex returned.
    home.rex_fault("rex-flaps", true);
    fs::remove_file(home.rex_socket()).unwrap();
    let _replacement = bind(&home);
    drop(original);
    assert!(soon(|| home.starts() > unconfirmed), "the stale subscription was not replaced");
    settle();
    assert!(home.health("Waiting for Rex"));
    assert_eq!(home.destination.texts(), [ENABLED, "`amp` **finished** · `original-context`"]);

    view(&home, B, "replacement-context");
    home.rex_fault("rex-flaps", false);
    home.wait_for_health("Listening");
    expect_message(&home, DISCORD, 2, RECOVERED);
    emit(&home, B, "error");
    expect_message(&home, DISCORD, 3, "`amp` **reported an error** · `replacement-context`");
    settle();
    assert_eq!(home.destination.requests().len(), 4);
}
