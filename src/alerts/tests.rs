//! Checks which Rex events become alerts, what each message says, and what is saved.
//!
//! The event lines are those a Rex 0.1.0-preview.21 server printed for
//! programs a test started in a session of its own; only the IDs are shortened.

use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, Instant};

use super::event::{self, Alert, Seen, Waiting};
use super::{
    Alerts, Content, Destination, Kind, Settings, forward, message, send, stand_in, valid_chat, valid_token, webhook,
};
use crate::fixture::Fixture;
use crate::json::Value;

const TOKEN: &str = "123456:SYNTHETICsyntheticSYNTHETIC00";
const LINK: &str = "https://discord.com/api/webhooks/1234567890/SYNTHETIC_synthetic-SYNTHETIC0";
const CAPABILITY: &str = "milorelay_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq";

/// A terminal event as `rex events --all --json` prints it.
fn line(block: &str, name: &str, payload: &str) -> String {
    format!(
        "{{\"type\":\"session_event\",\"payload\":{{\"session_id\":\"session:01a1\",\"event\":{{\"type\":\"block_event\",\
         \"payload\":{{\"block_id\":\"block:{block}\",\"creator_name\":\"com.superlogical.terminal\",\"name\":\"{name}\",\
         \"payload\":{payload}}}}}}}}}"
    )
}

fn status(block: &str, record: &str) -> String {
    line(block, "program_status_changed", &format!("{{\"id\":\"\",\"record\":{record},\"reason\":\"report\"}}"))
}

fn blocked(block: &str, kind: &str, msg: &str) -> String {
    status(
        block,
        &format!(
            "{{\"state\":\"blocked\",\"kind\":\"{kind}\",\"app\":\"synthetic\",\"effective_app\":\"synthetic\",\
             \"msg\":\"{msg}\",\"updated_at\":\"2026-11-21T07:46:38.125362+04:00\",\"owner\":{{\"pid\":55644,\"name\":\"bash\"}}}}"
        ),
    )
}

fn working(block: &str) -> String {
    status(block, "{\"state\":\"working\",\"app\":\"synthetic\",\"updated_at\":\"2026-11-21T07:46:37Z\"}")
}

/// The alerts a sequence of lines amounts to, each `seconds` after the last.
fn alerts(seen: &mut Seen, lines: &[String], seconds: u64) -> Vec<Alert> {
    let start = Instant::now();
    lines
        .iter()
        .zip(0..)
        .filter_map(|(line, step)| {
            seen.alert(event::parse(line.as_bytes())?, start + Duration::from_secs(seconds * step))
        })
        .collect()
}

fn kinds(alerts: &[Alert]) -> Vec<Kind> {
    alerts.iter().map(|alert| alert.kind).collect()
}

#[test]
fn only_the_events_rex_raises_as_alerts_become_alerts() {
    let lines = [
        line("a", "bell", "{}"),
        line("b", "desktop_notification", "{\"title\":\"\",\"body\":\"SYNTHETIC osc9 body\"}"),
        blocked("c", "permission", "SYNTHETIC approve?"),
        status("d", "{\"id\":\"job/1\",\"state\":\"done\",\"app\":\"synthetic\",\"title\":\"SYNTH task\",\"msg\":\"SYNTHETIC done\",\"updated_at\":\"x\"}"),
        status("e", "{\"state\":\"error\",\"effective_app\":\"synthetic\",\"msg\":\"SYNTHETIC failed\",\"updated_at\":\"x\"}"),
        // What a session or a program does that is no alert.
        working("f"),
        status("f", "{\"state\":\"idle\",\"updated_at\":\"x\"}"),
        status("f", "{\"state\":\"a-state-from-a-later-rex\",\"updated_at\":\"x\"}"),
        line("f", "progress_report", "{\"state\":\"set\",\"progress\":40}"),
        line("f", "child_exited", "{\"exit_code\":3,\"error\":\"exit status 3\"}"),
        line("f", "process_changed", "{\"generation\":2}"),
        line("f", "title_changed", "{\"title\":\"SYNTHETIC title\"}"),
        "{\"type\":\"session_event\",\"payload\":{\"session_id\":\"session:01a1\",\"event\":{\"type\":\"session_view_changed\",\"payload\":{\"revision\":9}}}}".into(),
        "{\"type\":\"client_joined\",\"payload\":{\"client\":{\"client_id\":\"c\"}}}".into(),
        "not json".into(),
    ];
    let found = alerts(&mut Seen::default(), &lines, 0);
    assert_eq!(kinds(&found), [Kind::Bell, Kind::Notification, Kind::Blocked, Kind::Done, Kind::Error]);
    assert_eq!(found[1].text, "SYNTHETIC osc9 body");
    assert_eq!((found[2].waiting, found[2].app.as_deref()), (Some(Waiting::Permission), Some("synthetic")));
    assert_eq!(found[3].text, "SYNTH task: SYNTHETIC done");
}

#[test]
fn another_kind_of_block_is_never_taken_for_a_terminal() {
    let other = line("a", "bell", "{}").replace("com.superlogical.terminal", "com.example.plugin");
    assert_eq!(event::parse(other.as_bytes()), None);
}

#[test]
fn a_status_rex_repeats_is_one_alert_until_the_program_moves_on() {
    let mut seen = Seen::default();
    // Rex sends the event for every report, even one that changes nothing.
    let repeats =
        [blocked("a", "permission", "one"), blocked("a", "permission", "one"), blocked("a", "permission", "two")];
    assert_eq!(kinds(&alerts(&mut seen, &repeats, 0)), [Kind::Blocked]);
    // Waiting for something else, or blocking again after working, is news.
    let moved = [blocked("a", "question", "q"), working("a"), blocked("a", "question", "q")];
    assert_eq!(kinds(&alerts(&mut seen, &moved, 0)), [Kind::Blocked, Kind::Blocked]);
    // Another terminal in the same state is its own alert.
    assert_eq!(kinds(&alerts(&mut seen, &[blocked("b", "question", "q")], 0)), [Kind::Blocked]);
}

#[test]
fn a_cleared_or_closed_terminal_can_alert_again() {
    let mut seen = Seen::default();
    let cleared = [
        blocked("a", "auth", "x"),
        line("a", "program_status_removed", "{\"ids\":[\"\"],\"reason\":\"prompt\"}"),
        blocked("a", "auth", "x"),
        "{\"type\":\"session_event\",\"payload\":{\"session_id\":\"s\",\"event\":{\"type\":\"block_closed\",\"payload\":{\"block_id\":\"block:a\"}}}}".into(),
        blocked("a", "auth", "x"),
    ];
    assert_eq!(alerts(&mut seen, &cleared, 0).len(), 3);
}

#[test]
fn bells_and_repeated_notifications_are_sent_once_in_a_while() {
    let bell = || line("a", "bell", "{}");
    let notice = |body: &str| line("a", "desktop_notification", &format!("{{\"title\":\"T\",\"body\":\"{body}\"}}"));
    // Ten seconds apart: the second and third fall in the first one's half minute.
    assert_eq!(alerts(&mut Seen::default(), &[bell(), bell(), bell(), bell()], 10).len(), 2);
    assert_eq!(alerts(&mut Seen::default(), &[bell(), line("b", "bell", "{}")], 0).len(), 2);
    let notices = [notice("same"), notice("same"), notice("other"), notice("other")];
    assert_eq!(alerts(&mut Seen::default(), &notices, 1).len(), 2);
    assert_eq!(alerts(&mut Seen::default(), &[notice("same"), notice("same")], 31).len(), 2);
}

fn alert(kind: Kind, text: &str) -> Alert {
    Alert { kind, waiting: Some(Waiting::Permission), app: Some("claude-code".into()), text: text.into() }
}

#[test]
fn nothing_a_program_wrote_is_sent_unless_text_was_chosen() {
    let secret = alert(Kind::Blocked, "Delete /Users/me/private.txt?");
    assert_eq!(
        message::text(&secret, Content::Metadata, 0, Destination::Telegram),
        "Rex: <code>claude-code</code> is waiting for permission"
    );
    assert_eq!(
        message::text(&secret, Content::Text, 2, Destination::Discord),
        "`claude-code` **is waiting for permission**\n`Delete /Users/me/private.txt?`\n(2 earlier alerts were not sent.)"
    );
    let unnamed = Alert { kind: Kind::Done, waiting: None, app: None, text: "x".into() };
    assert_eq!(message::text(&unnamed, Content::Metadata, 0, Destination::Discord), "a program **finished**");
    let bell = message::text(&alert(Kind::Bell, ""), Content::Text, 0, Destination::Telegram);
    assert_eq!(bell, "Rex: a terminal rang its bell");
}

#[test]
fn a_program_s_name_and_text_cannot_become_a_link_or_mark_up() {
    // A name the protocol allows can look like an address to sign in at.
    let lure = Alert {
        kind: Kind::Blocked,
        waiting: Some(Waiting::Auth),
        app: Some("login.example.com".into()),
        text: String::new(),
    };
    assert_eq!(
        message::text(&lure, Content::Metadata, 0, Destination::Telegram),
        "Rex: <code>login.example.com</code> is waiting for you to sign in"
    );
    assert_eq!(
        message::text(&lure, Content::Metadata, 0, Destination::Discord),
        "`login.example.com` **is waiting for you to sign in**"
    );
    let text = "</code><a href=\"https://example.com\">x</a> & `[label](https://example.com)` ||s||";
    let telegram = message::text(&alert(Kind::Error, text), Content::Text, 0, Destination::Telegram);
    assert_eq!(
        telegram.lines().nth(1).unwrap(),
        "<code>&lt;/code&gt;&lt;a href=\"https://example.com\"&gt;x&lt;/a&gt; &amp; `[label](https://example.com)` ||s||</code>"
    );
    // Nothing in the text can end Discord's code span, the only thing that could.
    let discord = message::text(&alert(Kind::Error, text), Content::Text, 0, Destination::Discord);
    let said = discord.lines().nth(1).unwrap();
    assert!(said.starts_with('`') && said.ends_with('`') && !said[1..said.len() - 1].contains('`'), "{said}");
}

#[test]
fn a_program_name_outside_the_protocol_s_characters_is_not_sent() {
    let named = |app: &str| {
        let record = format!("{{\"state\":\"done\",\"app\":\"{app}\",\"updated_at\":\"x\"}}");
        alerts(&mut Seen::default(), &[status("a", &record)], 0).remove(0).app
    };
    assert_eq!(named("cargo.build+1").as_deref(), Some("cargo.build+1"));
    assert_eq!(named("@everyone"), None);
    assert_eq!(named("two words"), None);
}

#[test]
fn program_text_is_one_short_line_that_mentions_nobody() {
    let said = |text: &str| message::text(&alert(Kind::Error, text), Content::Text, 0, Destination::Discord);
    let line = said("ping @everyone <@123> and @here\nsecond\tline\u{1b}[31m \u{202e}reversed");
    assert!(!line.contains("@everyone") && !line.contains("<@123>") && !line.contains("@here"), "{line:?}");
    assert_eq!(line.matches("@\u{200b}").count(), 3);
    assert_eq!(line.lines().count(), 2, "{line:?}");
    assert!(!line.chars().any(|c| c == '\u{1b}' || c == '\u{202e}'));
    // A break a program put in itself cannot make a mention whole again.
    assert_eq!(said("@\u{200b}here"), "`claude-code` **reported an error**\n`@\u{200b} here`");
    // Neither can it pad a line with blanks that are not spaces until it looks like two.
    assert_eq!(said("a\u{3164}\u{2800}\u{e0020}\u{61c}b"), "`claude-code` **reported an error**\n`a b`");
    let long = said(&"é".repeat(1000));
    assert_eq!(long.lines().nth(1).unwrap().chars().count(), 303);
    assert!(long.ends_with("…`"));
}

fn field<'a>(body: &'a str, name: &str) -> Value<'a> {
    let [value] = Value::parse(body.as_bytes()).expect("the body is JSON").fields([name]);
    value.unwrap_or_else(|| panic!("no {name} in {body}"))
}

#[test]
fn each_destination_is_told_how_to_read_the_text_and_to_notify_nobody_by_name() {
    let text = "Rex: \"quoted\" \\ back\nline";
    let discord = message::body(Destination::Discord, "", text, Some(Kind::Blocked));
    let mut embeds = field(&discord, "embeds").elements().unwrap();
    let [author, description, color, timestamp] = embeds.next().unwrap().fields(["author", "description", "color", "timestamp"]);
    let [name, icon] = author.unwrap().fields(["name", "icon_url"]);
    assert!(name.unwrap().is("milo - Needs attention"));
    assert!(icon.unwrap().is("https://usemilo.net/apple-touch-icon.png"));
    let stamp = timestamp.unwrap().string(20).unwrap();
    assert!(stamp.len() == 20 && stamp.ends_with('Z') && stamp.as_bytes()[10] == b'T');
    assert_eq!(description.unwrap().string(100).unwrap(), text);
    assert_eq!(color.unwrap().unsigned(), Some(0xe5a53b));
    assert!(embeds.next().is_none());
    let [parse] = field(&discord, "allowed_mentions").fields(["parse"]);
    assert_eq!(parse.unwrap().elements().unwrap().count(), 0);
    assert!(Value::parse(discord.as_bytes()).unwrap().fields(["content", "flags"]).iter().all(Option::is_none));

    let group = message::body(Destination::Telegram, "-1001234567890", text, None);
    assert_eq!(field(&group, "chat_id").integer(), Some(-1_001_234_567_890));
    assert_eq!(field(&group, "text").string(100).unwrap(), text);
    let [off] = field(&group, "link_preview_options").fields(["is_disabled"]);
    assert_eq!(off.unwrap().boolean(), Some(true));
    // Telegram reads the text as HTML, in which a program's words are marked as code.
    assert!(field(&group, "parse_mode").is("HTML"));
    let channel = message::body(Destination::Telegram, "@synthetic_channel", text, None);
    assert_eq!(field(&channel, "chat_id").string(40).unwrap(), "@synthetic_channel");
}

#[test]
fn mirroring_is_off_and_sends_no_text_until_chosen() {
    let settings = Settings::default();
    assert!(!settings.enabled && settings.destination.is_none() && settings.content == Content::Metadata);
    assert_eq!(Destination::ALL.map(|destination| settings.active(destination)), [false; 2]);
    assert_eq!(Kind::ALL.map(|kind| settings.mirrors(kind)), [true, true, true, false, false]);
    assert_eq!(Settings::parse("").unwrap(), settings);
}

#[test]
fn saved_settings_read_back_and_a_broken_file_mirrors_nothing() {
    let chosen = Settings {
        enabled: true,
        destination: Some(Destination::Telegram),
        targets: None,
        kinds: [true, false, false, true, true],
        content: Content::Text,
        telegram_chat: "@synthetic_channel".into(),
    };
    assert_eq!(Settings::parse(&chosen.render()).unwrap(), chosen);
    assert_eq!(Destination::ALL.map(|destination| chosen.active(destination)), [true, false]);
    assert!(!chosen.render().contains("targets"));
    assert_eq!(Settings::parse("events = \n").unwrap().kinds, [false; 5]);
    // A later version's setting is left alone.
    assert_eq!(Settings::parse("quiet_hours = 22-7\n").unwrap(), Settings::default());
    for broken in [
        "enabled = yes",
        "destination = email",
        "targets = telegram,email",
        "events = blocked,everything",
        "content = all",
        "telegram_chat = a b",
        "enabled",
    ] {
        assert!(Settings::parse(broken).is_err(), "{broken}");
    }
}

#[test]
fn an_explicit_target_list_overrides_the_legacy_selection() {
    let prefix = "enabled = true\ndestination = discord\n";
    let legacy = Settings::parse(prefix).unwrap();
    assert_eq!(Destination::ALL.map(|destination| legacy.active(destination)), [false, true]);
    let explicit = Settings::parse(&format!("{prefix}targets = telegram, discord\n")).unwrap();
    assert_eq!(Destination::ALL.map(|destination| explicit.active(destination)), [true, true]);
    assert_eq!(Settings::parse(&explicit.render()).unwrap(), explicit);
    let empty = Settings::parse(&format!("{prefix}targets =\n")).unwrap();
    assert_eq!(Destination::ALL.map(|destination| empty.active(destination)), [false; 2]);
}

#[test]
fn only_a_bot_token_a_chat_and_discord_s_own_webhook_links_are_accepted() {
    assert!(valid_token(TOKEN));
    for token in [
        "",
        "123456",
        "123456:short",
        "abc:SYNTHETICsyntheticSYNTHETIC00",
        "123456:SYNTHETIC synthetic SYNTHETIC00",
        "123456:SYNTHETICsyntheticSYNTHETIC00/../x",
    ] {
        assert!(!valid_token(token), "{token}");
    }
    for chat in ["-1001234567890", "42", "@synthetic_channel"] {
        assert!(valid_chat(chat), "{chat}");
    }
    for chat in ["", "-", "@ab", "@1channel", "12a", "@name/../x", "--1"] {
        assert!(!valid_chat(chat), "{chat}");
    }
    assert_eq!(webhook(LINK), Some("1234567890/SYNTHETIC_synthetic-SYNTHETIC0"));
    assert!(webhook(&LINK.replace("discord.com", "discordapp.com")).is_some());
    for link in [
        LINK.replace("https://discord.com", "https://discord.com.example.org"),
        LINK.replace("https://discord.com", "https://example.org"),
        LINK.replace("https://", "http://"),
        format!("{LINK}?wait=true"),
        format!("{LINK}/github"),
        "https://discord.com/api/webhooks/1234567890/../../users".into(),
    ] {
        assert_eq!(webhook(&link), None, "{link}");
    }
}

#[test]
fn only_a_stand_in_on_this_machine_can_replace_a_destination() {
    assert_eq!(stand_in("http://127.0.0.1:8080"), Some("http://127.0.0.1:8080"));
    for other in [
        "http://127.0.0.1:8080/x",
        "http://127.0.0.1:8080@example.org",
        "https://example.org",
        "http://localhost:80",
        "http://127.0.0.1:99999",
        "http://127.0.0.1:",
    ] {
        assert_eq!(stand_in(other), None, "{other}");
    }
}

#[test]
fn the_request_speaks_only_https_and_keeps_its_body_whole() {
    let body = message::body(Destination::Discord, "", "say \"hi\" \\ @\u{200b}here", Some(Kind::Error));
    let request = send::request("https://discord.com/api/webhooks/1/abc", &body).options;
    assert!(request.contains("proto = \"=https\"\n") && request.contains("max-redirs = 0\n"), "{request}");
    assert!(request.starts_with("url = \"https://discord.com/api/webhooks/1/abc\"\n"));
    // curl reads the value back as it was written: one line, quotes and backslashes escaped.
    let data = request.lines().find_map(|line| line.strip_prefix("data-raw = \"")).unwrap().strip_suffix('"').unwrap();
    assert_eq!(data.replace("\\\"", "\"").replace("\\\\", "\\"), body);
    assert!(send::request("http://127.0.0.1:9/x", "{}").options.contains("proto = \"=http\"\n"));
}

#[test]
fn both_destinations_ways_of_saying_how_long_to_wait_are_understood() {
    let telegram = "{\"ok\":false,\"error_code\":429,\"description\":\"Too Many Requests: retry after 35\",\"parameters\":{\"retry_after\":35}}";
    assert_eq!(send::retry_after(telegram), Duration::from_secs(35));
    let discord = "{\"message\":\"You are being rate limited.\",\"retry_after\":64.57,\"global\":false}";
    assert_eq!(send::retry_after(discord), Duration::from_secs_f64(64.57));
    for unclear in ["", "<html>", "{\"retry_after\":-1}", "{\"retry_after\":1e99}"] {
        assert_eq!(send::retry_after(unclear), Duration::from_secs(5), "{unclear}");
    }
}

#[test]
fn a_rejected_hosted_telegram_pairing_prompts_reconnection_not_a_new_token() {
    let refused = send::Outcome::Refused(401);
    assert_eq!(
        refused.reason(Destination::Telegram, true),
        "Telegram pairing expired or was revoked; reconnect with Milo"
    );
    assert!(refused.reason(Destination::Telegram, false).contains("refused the saved secret"));
}

#[test]
fn a_line_too_long_to_be_an_alert_is_skipped_without_losing_the_next() {
    let huge = line("a", "clipboard_written", &format!("{{\"text\":\"{}\"}}", "x".repeat(200_000)));
    let stream = format!("{huge}\n{}\n{}", line("a", "bell", "{}"), line("b", "bell", "{}"));
    let mut kinds = Vec::new();
    forward::read(stream.as_bytes(), [true; 5], |alert| kinds.push(alert.kind));
    assert_eq!(kinds, [Kind::Bell, Kind::Bell]);
}

#[test]
fn what_is_an_alert_is_decided_as_each_event_arrives_and_only_chosen_kinds_wait() {
    // Had the reports between waited behind a slow delivery and been dropped,
    // the second block would look like a repeat of the first.
    let lines =
        [blocked("a", "permission", "x"), working("a"), line("a", "bell", "{}"), blocked("a", "permission", "x")];
    let mut kinds = Vec::new();
    forward::read(lines.join("\n").as_bytes(), [true, true, true, false, false], |alert| kinds.push(alert.kind));
    assert_eq!(kinds, [Kind::Blocked, Kind::Blocked]);
}

#[test]
fn rex_is_reached_for_again_for_ten_minutes_however_long_it_was_followed() {
    let start = Instant::now();
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    let mut away = forward::Away::new();
    // A stream followed for two hours ends: the wait for Rex begins now, not then.
    assert_eq!(away.wait(Duration::from_secs(7200), at(0)), Some(Duration::from_secs(1)));
    assert_eq!(away.wait(Duration::ZERO, at(1)), Some(Duration::from_secs(2)));
    for (seconds, wait) in [(3, 4), (7, 8), (15, 16), (31, 30), (61, 30), (599, 30)] {
        assert_eq!(away.wait(Duration::ZERO, at(seconds)), Some(Duration::from_secs(wait)), "{seconds}");
    }
    assert_eq!(away.wait(Duration::ZERO, at(600)), None);
    // Rex came back and was followed for a while: the next absence is a new one.
    assert_eq!(away.wait(Duration::from_secs(90), at(700)), Some(Duration::from_secs(1)));
}

fn store(f: &Fixture) -> Alerts {
    Alerts::at(&f.root.join("config"), &f.root.join("state"))
}

#[test]
fn turning_on_needs_a_saved_destination_and_says_what_will_be_sent() {
    let f = Fixture::folder();
    let alerts = store(&f);
    assert!(alerts.enable().unwrap_err().contains("Nothing was turned on"));
    assert!(alerts.choose(Destination::Discord).unwrap_err().contains("No Discord destination is saved"));
    assert!(!alerts.settings().unwrap().enabled);

    assert!(alerts.set_telegram("not a token", "-1001234567890").is_err());
    assert!(alerts.set_telegram(TOKEN, "my chat").is_err());
    assert!(alerts.set_discord("https://example.org/api/webhooks/1234567890/SYNTHETIC_synthetic-SYNTHETIC0").is_err());
    let summary = alerts.summary().unwrap();
    assert_eq!(summary.saved, [false, false]);

    assert!(alerts.set_telegram(TOKEN, "-1001234567890").unwrap().contains("still off"));
    let on = alerts.enable().unwrap();
    assert!(on.contains("Telegram chat -1001234567890") && on.contains("No text from the program"), "{on}");
    assert!(on.contains("Rex's own notifications are unchanged") && on.contains("milo alerts off"), "{on}");
    assert!(alerts.set_content(Content::Text).unwrap().contains("up to 300 characters of the text"));
    assert!(alerts.settings().unwrap().enabled);
    assert!(alerts.disable().unwrap().contains("off"));
    assert!(!alerts.settings().unwrap().enabled);
}

#[test]
fn a_secret_is_kept_apart_from_the_settings_in_a_file_only_the_user_can_read() {
    let f = Fixture::folder();
    let alerts = store(&f);
    alerts.set_telegram(TOKEN, "@synthetic_channel").unwrap();
    alerts.set_discord(LINK).unwrap();
    let settings = std::fs::read_to_string(f.root.join("config/alerts.conf")).unwrap();
    assert!(!settings.contains("SYNTHETIC") && settings.contains("telegram_chat = @synthetic_channel"), "{settings}");
    let secrets = f.root.join("state/alerts-secrets");
    assert_eq!(std::fs::metadata(&secrets).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(alerts.summary().unwrap().saved, [true, true]);
    assert_eq!(alerts.settings().unwrap().destination, Some(Destination::Discord));

    // Forgetting the selected destination also turns mirroring off.
    alerts.enable().unwrap();
    alerts.forget(Destination::Discord).unwrap();
    let after = alerts.settings().unwrap();
    assert!(!after.enabled && after.destination.is_none());
    assert_eq!(alerts.summary().unwrap().saved, [true, false]);
    alerts.forget(Destination::Telegram).unwrap();
    assert!(!secrets.exists());
    assert!(alerts.settings().unwrap().telegram_chat.is_empty());
}

#[test]
fn a_secrets_file_others_can_read_is_refused() {
    let f = Fixture::folder();
    let alerts = store(&f);
    alerts.set_discord(LINK).unwrap();
    let secrets = f.root.join("state/alerts-secrets");
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(alerts.summary().unwrap().saved, [false, false]);
    assert!(alerts.enable().unwrap_err().contains("Could not read the saved secret"));
}

#[test]
fn saving_another_destination_while_mirroring_does_not_move_the_alerts() {
    let f = Fixture::folder();
    let alerts = store(&f);
    alerts.set_telegram(TOKEN, "-1001234567890").unwrap();
    alerts.enable().unwrap();
    let saved = alerts.set_discord(LINK).unwrap();
    assert!(saved.contains("Alerts still go to Telegram chat -1001234567890"), "{saved}");
    assert_eq!(alerts.settings().unwrap().destination, Some(Destination::Telegram));
    // Changing the recipient disarms the active destination.
    alerts.set_telegram(TOKEN, "@synthetic_channel").unwrap();
    assert_eq!(alerts.settings().unwrap().telegram_chat, "@synthetic_channel");
    assert!(!alerts.settings().unwrap().active(Destination::Telegram));
    assert!(alerts.choose(Destination::Discord).unwrap().contains("the Discord channel"));
    assert_eq!(alerts.settings().unwrap().destination, Some(Destination::Discord));
}

#[test]
fn scoped_forwarding_and_legacy_selection_do_not_change_each_other() {
    let f = Fixture::folder();
    let alerts = store(&f);
    let (telegram, discord) = (Destination::Telegram, Destination::Discord);
    let active = || {
        let settings = alerts.settings().unwrap();
        Destination::ALL.map(|destination| settings.active(destination))
    };
    assert!(alerts.set_forwarding(discord, true).is_err());
    assert!(alerts.test_destination(discord).unwrap_err().contains("No Discord destination is saved"));
    assert_eq!(active(), [false; 2]);

    alerts.set_telegram(TOKEN, "-1001234567890").unwrap();
    alerts.enable().unwrap();
    alerts.set_discord(LINK).unwrap();
    assert_eq!(active(), [true, false]);
    alerts.set_forwarding(discord, true).unwrap();
    assert_eq!(alerts.settings().unwrap().destination, Some(telegram));
    assert_eq!(active(), [true, true]);

    alerts.disable().unwrap();
    assert_eq!(active(), [false; 2]);
    alerts.enable().unwrap();
    assert_eq!(active(), [true, true]);
    alerts.set_forwarding(discord, false).unwrap();
    alerts.choose(discord).unwrap();
    assert_eq!(alerts.settings().unwrap().destination, Some(discord));
    assert_eq!(active(), [true, false]);
    assert_eq!(alerts.summary().unwrap().saved, [true, true]);

    alerts.set_forwarding(discord, true).unwrap();
    alerts.disable().unwrap();
    alerts.set_forwarding(telegram, true).unwrap();
    assert_eq!(active(), [true, false]);
}

#[test]
fn manual_replacement_disarms_only_the_changed_destination() {
    const NEW_TOKEN: &str = "654321:SYNTHETICsyntheticSYNTHETIC11";
    const NEW_LINK: &str = "https://discord.com/api/webhooks/1234567891/SYNTHETIC_synthetic-SYNTHETIC1";
    let f = Fixture::folder();
    let alerts = store(&f);
    let (telegram, discord) = (Destination::Telegram, Destination::Discord);
    let active = || {
        let settings = alerts.settings().unwrap();
        Destination::ALL.map(|destination| settings.active(destination))
    };
    alerts.set_telegram(TOKEN, "42").unwrap();
    alerts.set_discord(LINK).unwrap();
    alerts.set_forwarding(telegram, true).unwrap();
    alerts.set_forwarding(discord, true).unwrap();
    alerts.set_telegram(TOKEN, "42").unwrap();
    alerts.set_discord(LINK).unwrap();
    assert_eq!(active(), [true, true]);

    alerts.set_telegram(TOKEN, "43").unwrap();
    assert_eq!(active(), [false, true]);
    alerts.set_forwarding(telegram, true).unwrap();
    alerts.set_telegram(NEW_TOKEN, "43").unwrap();
    assert_eq!(active(), [false, true]);
    alerts.set_forwarding(telegram, true).unwrap();
    alerts.set_discord(NEW_LINK).unwrap();
    assert_eq!(active(), [true, false]);

    alerts.set_forwarding(discord, true).unwrap();
    alerts.disable().unwrap();
    alerts.set_discord(LINK).unwrap();
    alerts.enable().unwrap();
    assert_eq!(active(), [true, false]);
}

#[test]
fn restoring_a_different_chat_cannot_reuse_a_verified_telegram_binding() {
    let f = Fixture::folder();
    let alerts = store(&f);
    let (telegram, discord) = (Destination::Telegram, Destination::Discord);
    alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
    alerts.set_forwarding(telegram, true).unwrap();
    alerts.set_discord(LINK).unwrap();
    alerts.set_forwarding(discord, true).unwrap();
    let verified = alerts.settings().unwrap();
    let secret = alerts.target_secret(&verified, telegram).unwrap();
    assert_eq!(secret, format!("{CAPABILITY}:42"));
    assert!(alerts.message_request(telegram, &secret, "42", "test", None).is_some());

    std::fs::write(alerts.file(), verified.render().replace("telegram_chat = 42", "telegram_chat = 43")).unwrap();
    let restored = alerts.settings().unwrap();
    assert_eq!(restored.telegram_chat, "43");
    assert!(alerts.set_forwarding(telegram, true).unwrap_err().contains("Connect Telegram again"));
    assert!(alerts.test_destination(telegram).unwrap_err().contains("Connect Telegram again"));
    assert!(alerts.message_request(telegram, &secret, &restored.telegram_chat, "test", None).is_none());
    assert!(restored.active(discord));
    assert!(alerts.target_secret(&restored, discord).is_ok());
}

#[test]
fn legacy_unbound_telegram_capability_requires_repair() {
    let f = Fixture::folder();
    let alerts = store(&f);
    let telegram = Destination::Telegram;
    alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
    alerts.secrets.set(telegram, CAPABILITY).unwrap();
    assert!(alerts.summary().unwrap().note.contains("Connect Telegram again"));
    assert!(alerts.enable().unwrap_err().contains("Connect Telegram again"));
    assert!(alerts.test_destination(telegram).unwrap_err().contains("Connect Telegram again"));
    assert!(alerts.message_request(telegram, CAPABILITY, "42", "test", None).is_none());
    assert!(alerts.set_telegram(CAPABILITY, "42").is_err());
}

#[test]
fn the_alert_transaction_holds_the_config_lock_through_a_credential_write() {
    let f = Fixture::folder();
    let alerts = Alerts::at(&f.root, &f.root);
    let probe = crate::fs_ops::open_dir(&f.root).unwrap();
    alerts
        .locked(|edit| {
            assert!(matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
            edit.save_discord(LINK, false)?;
            assert!(matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
            Ok(())
        })
        .unwrap();
    // A program another test is starting holds a copy of the lock's descriptor
    // until it begins to run, so the lock can outlast the transaction by a moment.
    let ended = Instant::now();
    while probe.try_lock().is_err() {
        assert!(ended.elapsed() < Duration::from_secs(5), "the lock outlived its transaction");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn failed_forget_revoke_never_rearms_the_forgotten_provider() {
    let f = Fixture::folder();
    let mut alerts = store(&f);
    let (telegram, discord) = (Destination::Telegram, Destination::Discord);
    alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
    alerts.set_discord(LINK).unwrap();
    alerts.set_forwarding(telegram, true).unwrap();
    alerts.set_forwarding(discord, true).unwrap();

    alerts.relay = None;
    assert!(alerts.forget(telegram).unwrap_err().contains("Could not disconnect"));
    let reopened = store(&f);
    reopened.enable().unwrap();
    let settings = reopened.settings().unwrap();
    assert_eq!(Destination::ALL.map(|destination| settings.active(destination)), [false, true]);
    assert_eq!(reopened.secret(telegram).unwrap(), format!("{CAPABILITY}:42"));
}

#[test]
fn failed_uninstall_revoke_leaves_mirroring_durably_disabled() {
    let f = Fixture::folder();
    let mut alerts = store(&f);
    let (telegram, discord) = (Destination::Telegram, Destination::Discord);
    alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
    alerts.set_discord(LINK).unwrap();
    alerts.set_forwarding(telegram, true).unwrap();
    alerts.set_forwarding(discord, true).unwrap();

    // No request can be sent in the wrong relay environment.
    alerts.relay = None;
    assert!(alerts.locked(|edit| edit.remove()).unwrap_err().contains("Could not disconnect"));
    let reopened = store(&f);
    assert!(!reopened.settings().unwrap().has_active());
    assert!(reopened.enable().is_err());
    assert_eq!(reopened.secret(telegram).unwrap(), format!("{CAPABILITY}:42"));
}

#[test]
fn hosted_replacement_and_forget_leave_the_other_provider_active() {
    let f = Fixture::folder();
    let alerts = store(&f);
    let (telegram, discord) = (Destination::Telegram, Destination::Discord);
    let active = || {
        let settings = alerts.settings().unwrap();
        Destination::ALL.map(|destination| settings.active(destination))
    };
    alerts.set_telegram(TOKEN, "-1001234567890").unwrap();
    alerts.set_hosted_discord(LINK).unwrap();
    assert_eq!(active(), [false; 2]);
    alerts.set_forwarding(telegram, true).unwrap();
    alerts.set_forwarding(discord, true).unwrap();
    assert_eq!(active(), [true, true]);

    const NEW_LINK: &str = "https://discord.com/api/webhooks/1234567891/SYNTHETIC_synthetic-SYNTHETIC1";
    alerts.set_hosted_discord(NEW_LINK).unwrap();
    assert_eq!(active(), [true, false]);
    assert_eq!(alerts.summary().unwrap().saved, [true, true]);

    alerts.set_forwarding(discord, true).unwrap();
    alerts.choose(telegram).unwrap();
    alerts.forget(telegram).unwrap();
    let settings = alerts.settings().unwrap();
    assert_eq!(settings.destination, None);
    assert!(settings.telegram_chat.is_empty());
    assert_eq!(active(), [false, true]);
    assert_eq!(alerts.summary().unwrap().saved, [false, true]);
}

#[test]
fn settings_that_cannot_be_read_do_not_keep_mirroring_on_or_a_secret_in_place() {
    let f = Fixture::folder();
    let alerts = store(&f);
    alerts.set_discord(LINK).unwrap();
    alerts.enable().unwrap();
    let file = f.root.join("config/alerts.conf");
    std::fs::write(&file, "enabled = true\nnot a setting\n").unwrap();
    assert!(alerts.settings().is_err());
    assert!(alerts.enable().is_err() && alerts.set_content(Content::Text).is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "enabled = true\nnot a setting\n");

    alerts.forget(Destination::Discord).unwrap();
    assert!(!f.root.join("state/alerts-secrets").exists());
    std::fs::write(&file, "enabled = true\nnot a setting\n").unwrap();
    alerts.disable().unwrap();
    assert!(!alerts.settings().unwrap().enabled);
}

/// The Keychain path, in a keychain file of the test's own that is on no search list.
#[cfg(target_os = "macos")]
#[test]
fn the_keychain_keeps_and_forgets_a_secret() {
    use super::secret::Store;
    let f = Fixture::folder();
    let keychain = f.root.join("test.keychain-db");
    let made = std::process::Command::new("/usr/bin/security")
        .args(["create-keychain", "-p", "test"])
        .arg(&keychain)
        .status()
        .unwrap();
    assert!(made.success());
    let store = Store::Keychain(Some(keychain));
    assert_eq!(store.get(Destination::Telegram).unwrap(), None);
    store.set(Destination::Telegram, TOKEN).unwrap();
    store.set(Destination::Telegram, "654321:SYNTHETICsyntheticSYNTHETIC11").unwrap();
    assert_eq!(store.get(Destination::Telegram).unwrap().as_deref(), Some("654321:SYNTHETICsyntheticSYNTHETIC11"));
    assert_eq!(store.get(Destination::Discord).unwrap(), None);
    assert!(store.set(Destination::Discord, "has \"quotes\" and spaces").is_err());
    store.remove(Destination::Telegram).unwrap();
    store.remove(Destination::Telegram).unwrap();
    assert_eq!(store.get(Destination::Telegram).unwrap(), None);
}
