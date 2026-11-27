//! Accounts asked about over the network, against a loopback port that
//! stands in for every provider. No test contacts a real service.

use std::io::{BufRead as _, BufReader};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use super::super::accounts::{Accounts, Currency, Failure, Figure, Kind, Reading, Sources, Status, Unit};
use super::profile::{micros, ready};
use super::*;
use crate::preferences::{UsageAccount, UsageAccounts};

const DEEPSEEK: &str = include_str!("accounts/deepseek-balance.json");
const OPENROUTER: &str = include_str!("accounts/openrouter-key.json");
const DEVIN: &str = include_str!("accounts/devin-consumption-daily.json");
const COPILOT: &str = include_str!("accounts/github-ai-credit-usage.json");
const KEY: &str = "sk-test-0123456789abcdef";
/// Devin's billing day that holds `NOW`, from 08:00 UTC to 08:00 UTC.
const DEVIN_DAY: (i64, i64) = (1_795_075_200, 1_795_161_600);

fn elapsed(start: Instant, seconds: i64) -> Instant {
    start + Duration::from_secs(u64::try_from(seconds).unwrap())
}

struct Canned {
    status: u16,
    header: &'static str,
    body: String,
}

/// A loopback server that answers each provider's requests with a canned
/// reply and keeps what it was sent.
struct Provider {
    port: u16,
    replies: Arc<Mutex<Vec<(&'static str, Canned)>>>,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Provider {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let replies = Arc::new(Mutex::new(Vec::<(&'static str, Canned)>::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (answers, heard, stopped) = (replies.clone(), seen.clone(), stop.clone());
        let thread = thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let mut head = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                while reader.read_line(&mut head).is_ok_and(|read| read > 0) && !head.ends_with("\r\n\r\n") {}
                let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                heard.lock().unwrap().push(head);
                let answers = answers.lock().unwrap();
                let canned = answers.iter().find(|(id, _)| path.starts_with(&format!("/{id}/"))).map(|(_, c)| c);
                let (status, header, body) = canned.map_or((404, "", ""), |c| (c.status, c.header, c.body.as_str()));
                if status == 0 {
                    // Simulate a connection that drops before any HTTP reply.
                    continue;
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Canned\r\nContent-Type: application/json\r\n{header}Content-Length: {}\r\n\
                    Connection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self { port, replies, seen, stop, thread: Some(thread) }
    }

    fn reply(&self, id: &'static str, status: u16, header: &'static str, body: &str) {
        let mut replies = self.replies.lock().unwrap();
        replies.retain(|(known, _)| *known != id);
        replies.push((id, Canned { status, header, body: body.to_owned() }));
    }

    /// The request heads sent for one provider.
    fn asked(&self, id: &str) -> Vec<String> {
        let prefix = format!("GET /{id}/");
        self.seen.lock().unwrap().iter().filter(|head| head.starts_with(&prefix)).cloned().collect()
    }

    fn requests(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn chosen(accounts: &[(UsageAccount, Show)]) -> UsageAccounts {
    let mut choices = UsageAccounts::default();
    for &(account, show) in accounts {
        choices[account] = show;
    }
    choices
}

fn sources(f: &Fixture, provider: &Provider, environment: &[(&'static str, &str)]) -> Sources {
    Sources::test(environment, f.root.join("config/usage-accounts.conf"), f.root.join("state/accounts"), provider.port)
}

/// A pane with no tool on disk and the given accounts turned on.
fn pane(f: &Fixture, sources: Sources, accounts: &[(UsageAccount, Show)]) -> Usage {
    let mut usage = automatic(f, NOW);
    usage.accounts = Accounts::with(sources, chosen(accounts));
    usage
}

fn status(usage: &Usage, which: UsageAccount, now: i64) -> Status {
    usage.accounts.iter().find(|account| account.which == which).unwrap().status(now)
}

/// The frame's rows without their margins, down to the last row with anything on it.
fn text(usage: &mut Usage, width: usize, height: usize) -> String {
    let rows = frame(usage, width, height);
    rows.iter().map(|row| row.trim()).collect::<Vec<_>>().join("\n").trim_end().to_owned()
}

fn dollars(kind: Kind, value: f64) -> Figure {
    Figure { kind, unit: Unit::Money(Currency::Usd), value }
}

fn balance(observed: i64) -> Reading {
    let figure = Figure { kind: Kind::Balance, unit: Unit::Money(Currency::Cny), value: 110.0 };
    Reading { observed, figure, details: [None, None], until: None, depleted: false }
}

#[test]
fn each_provider_is_asked_its_documented_question_and_its_answer_keeps_its_own_unit() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    provider.reply("deepseek", 200, "", DEEPSEEK);
    provider.reply("openrouter", 200, "", OPENROUTER);
    provider.reply("devin", 200, "", DEVIN);
    provider.reply("copilot", 200, "", COPILOT);
    let environment = [
        ("MILO_DEEPSEEK_API_KEY", KEY),
        ("MILO_OPENROUTER_API_KEY", "sk-or-test-key-1"),
        ("MILO_DEVIN_API_KEY", "cog_test_key_22"),
        ("MILO_DEVIN_ORG", "org-abc123def456"),
        ("MILO_GITHUB_TOKEN", "github_pat_test_333"),
        ("MILO_GITHUB_USER", "example-user"),
    ];
    let every = UsageAccount::ALL.map(|account| (account, Show::Auto));
    let mut usage = pane(&f, sources(&f, &provider, &environment), &every);
    usage.accounts.settle(NOW, false);

    let asked = |id: &str| {
        let heads = provider.asked(id);
        assert_eq!(heads.len(), 1, "{id}: {heads:?}");
        heads.into_iter().next().unwrap()
    };
    let deepseek = asked("deepseek");
    assert!(deepseek.starts_with("GET /deepseek/user/balance HTTP/1.1\r\n"), "{deepseek}");
    assert!(deepseek.contains(&format!("Authorization: Bearer {KEY}\r\n")), "{deepseek}");
    assert!(asked("openrouter").starts_with("GET /openrouter/api/v1/key HTTP/1.1\r\n"));
    let devin = asked("devin");
    let (start, end) = DEVIN_DAY;
    let day = format!("/v3/organizations/org-abc123def456/consumption/daily?time_after={start}&time_before={end} ");
    assert!(devin.contains(&day) && devin.contains("Bearer cog_test_key_22\r\n"), "{devin}");
    let copilot = asked("copilot");
    assert!(copilot.starts_with("GET /copilot/users/example-user/settings/billing/ai_credit/usage HTTP/1.1"));
    assert!(copilot.contains("X-GitHub-Api-Version: 2026-03-10\r\n") && copilot.contains("vnd.github+json"));
    // Each key goes only to its own provider.
    assert!(provider.seen.lock().unwrap().iter().all(|head| head.matches("Bearer ").count() == 1));
    assert!(!devin.contains(KEY) && !copilot.contains(KEY) && !deepseek.contains("cog_"));

    assert_eq!(
        text(&mut usage, 80, 4),
        "DeepSeek · ¥110.00 balance · ¥10.00 granted\nOpenRouter · $74.50 key limit left · $1.25 today · $25.50 this month\n\
        Devin · 12.5 ACUs today\nCopilot · 1240 AI credits this month"
    );
    // The period a count covers ends with the provider's day or month, not with Milo's.
    let Status::Reading(devin, None) = status(&usage, UsageAccount::Devin, NOW) else { panic!() };
    assert_eq!(devin.until, Some(end));
    let Status::Reading(copilot, None) = status(&usage, UsageAccount::Copilot, NOW) else { panic!() };
    assert_eq!(copilot.until, Some(1_796_083_200));

    // What is saved for other panes holds figures and times, and no credential.
    let entries: Vec<_> = fs::read_dir(f.root.join("state/accounts")).unwrap().map(|entry| entry.unwrap()).collect();
    assert_eq!(entries.len(), UsageAccount::ALL.len());
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_str().unwrap();
        let saved = fs::read_to_string(entry.path()).unwrap();
        assert!(Value::parse(saved.as_bytes()).is_some(), "{saved}");
        assert!(environment.iter().all(|(_, value)| !saved.contains(value) && !name.contains(value)));
    }
}

#[test]
fn an_account_is_never_asked_about_until_it_is_turned_on() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    provider.reply("deepseek", 200, "", DEEPSEEK);
    let environment = [("MILO_DEEPSEEK_API_KEY", KEY), ("MILO_OPENROUTER_API_KEY", KEY)];
    let mut usage = pane(&f, sources(&f, &provider, &environment), &[]);
    usage.refresh(NOW, Instant::now(), true);
    usage.accounts.settle(NOW, true);
    assert_eq!(provider.requests(), 0);
    assert!(usage.accounts.iter().next().is_none() && usage.external_fd().is_none());
    assert_eq!(text(&mut usage, 60, 3), "No usage to show yet");
    assert!(!f.root.join("state/accounts").exists());

    // Turning one on asks that provider alone, though another key is in the environment.
    let mut usage = pane(&f, sources(&f, &provider, &environment), &[(UsageAccount::DeepSeek, Show::Auto)]);
    usage.accounts.settle(NOW, false);
    assert_eq!((provider.asked("deepseek").len(), provider.requests()), (1, 1));
    assert_eq!(text(&mut usage, 60, 1), "DeepSeek · ¥110.00 balance · ¥10.00 granted");
}

#[test]
fn a_missing_or_unsafe_key_is_reported_without_a_request() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    provider.reply("deepseek", 200, "", DEEPSEEK);
    let deepseek = UsageAccount::DeepSeek;
    let failed = |environment: &[(&'static str, &str)], show| {
        let mut usage = pane(&f, sources(&f, &provider, environment), &[(deepseek, show)]);
        usage.accounts.settle(NOW, false);
        let status = status(&usage, deepseek, NOW);
        (status, text(&mut usage, 60, 3))
    };

    // Auto gives no place to an account without a figure; with nothing else to show, the pane says why.
    let (status, rows) = failed(&[], Show::Auto);
    assert_eq!(status, Status::Failed(Failure::NoKey));
    assert_eq!(rows, "No usage to show yet\nDeepSeek · no key set\nRun milo doctor");
    let (_, rows) = failed(&[], Show::Always);
    assert_eq!(rows, "DeepSeek · no key set\nRun milo doctor");

    // A key that could alter the request is not sent at all.
    for key in ["short", "has space in it", "quote\"inside-key", "back\\slash-key"] {
        assert_eq!(
            failed(&[("MILO_DEEPSEEK_API_KEY", key)], Show::Always).0,
            Status::Failed(Failure::BadSetting),
            "{key}"
        );
    }

    // The settings file is read only while it is the user's private plain file.
    let file = f.root.join("config/usage-accounts.conf");
    f.write("config/usage-accounts.conf", &format!("# keys for milo usage\nMILO_DEEPSEEK_API_KEY = {KEY}\n"));
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    let (status, rows) = failed(&[], Show::Always);
    assert_eq!(status, Status::Failed(Failure::OpenFile));
    assert!(rows.starts_with("DeepSeek · settings file not private"), "{rows}");
    let target = f.root.join("config/elsewhere");
    fs::rename(&file, &target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&target, &file).unwrap();
    assert_eq!(failed(&[], Show::Always).0, Status::Failed(Failure::OpenFile));
    assert_eq!(provider.requests(), 0);

    fs::remove_file(&file).unwrap();
    fs::rename(&target, &file).unwrap();
    let (status, _) = failed(&[], Show::Auto);
    assert!(matches!(status, Status::Reading(..)), "{status:?}");
    assert!(provider.asked("deepseek")[0].contains(&format!("Bearer {KEY}\r\n")));

    // The environment wins, so one shell can use another key without editing the file.
    fs::remove_dir_all(f.root.join("state")).unwrap();
    failed(&[("MILO_DEEPSEEK_API_KEY", "sk-from-environment")], Show::Auto);
    assert!(provider.asked("deepseek")[1].contains("Bearer sk-from-environment\r\n"));
}

#[test]
fn a_private_file_error_does_not_hide_environment_accounts_or_survive_the_next_look() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    for (id, reply) in [("deepseek", DEEPSEEK), ("devin", DEVIN), ("copilot", COPILOT)] {
        provider.reply(id, 200, "", reply);
    }
    let file = "config/usage-accounts.conf";
    f.write(file, "MILO_DEVIN_ORG=org-abc123\nMILO_GITHUB_TOKEN=github_pat_test_333\nMILO_GITHUB_USER=example-user\n");
    fs::set_permissions(f.root.join(file), fs::Permissions::from_mode(0o644)).unwrap();
    let environment = [("MILO_DEEPSEEK_API_KEY", KEY), ("MILO_DEVIN_API_KEY", "cog_test_key_22")];
    let accounts = [UsageAccount::DeepSeek, UsageAccount::Devin, UsageAccount::Copilot].map(|a| (a, Show::Always));
    let mut usage = pane(&f, sources(&f, &provider, &environment), &accounts);
    usage.accounts.settle(NOW, false);
    assert!(matches!(status(&usage, UsageAccount::DeepSeek, NOW), Status::Reading(..)));
    for which in [UsageAccount::Devin, UsageAccount::Copilot] {
        assert_eq!(status(&usage, which, NOW), Status::Failed(Failure::OpenFile));
    }
    assert_eq!(provider.requests(), 1);

    fs::set_permissions(f.root.join(file), fs::Permissions::from_mode(0o600)).unwrap();
    usage.accounts.settle(NOW + 2, false);
    let devin = &provider.asked("devin")[0];
    assert!(devin.contains("/organizations/org-abc123/") && devin.contains("Bearer cog_test_key_22\r\n"));
    let copilot = &provider.asked("copilot")[0];
    assert!(copilot.contains("/users/example-user/") && copilot.contains("Bearer github_pat_test_333\r\n"));
    assert_eq!(provider.requests(), 3);
}

#[test]
fn an_account_named_by_its_own_setting_needs_that_setting_in_a_form_that_stays_inside_the_path() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    provider.reply("devin", 200, "", DEVIN);
    provider.reply("copilot", 200, "", COPILOT);
    let look = |environment: &[(&'static str, &str)], which| {
        let mut usage = pane(&f, sources(&f, &provider, environment), &[(which, Show::Always)]);
        usage.accounts.settle(NOW, false);
        status(&usage, which, NOW)
    };
    let devin = [("MILO_DEVIN_API_KEY", "cog_test_key_22")];
    assert_eq!(look(&devin, UsageAccount::Devin), Status::Failed(Failure::NoScope));
    for org in ["abc123", "org-", "org-a/../b", "org-a?x=1", "org-a b"] {
        let named = [devin[0], ("MILO_DEVIN_ORG", org)];
        assert_eq!(look(&named, UsageAccount::Devin), Status::Failed(Failure::BadSetting), "{org}");
    }
    for user in ["a/b", "name?x", "", "user name"] {
        let named = [("MILO_GITHUB_TOKEN", "github_pat_test_333"), ("MILO_GITHUB_USER", user)];
        let expected = if user.is_empty() { Failure::NoScope } else { Failure::BadSetting };
        assert_eq!(look(&named, UsageAccount::Copilot), Status::Failed(expected), "{user:?}");
    }
    assert_eq!(provider.requests(), 0);
}

#[test]
fn a_rate_limit_is_honoured_even_by_a_reload_and_the_last_answer_stays_with_its_age() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    let deepseek = UsageAccount::DeepSeek;
    provider.reply("deepseek", 200, "", DEEPSEEK);
    let mut usage = pane(&f, sources(&f, &provider, &[("MILO_DEEPSEEK_API_KEY", KEY)]), &[(deepseek, Show::Auto)]);
    let start = Instant::now();
    let clock = |now| elapsed(start, now - NOW);
    usage.accounts.settle_at(NOW, clock(NOW), false);
    let asked = || provider.asked("deepseek").len();
    assert_eq!(asked(), 1);

    usage.accounts.settle_at(NOW + 1, clock(NOW + 1), false);
    // A reload may ask again, but not within half a minute of the last request.
    usage.accounts.settle_at(NOW + 10, clock(NOW + 10), true);
    usage.accounts.settle_at(NOW + 30, clock(NOW + 30), false);
    assert_eq!(asked(), 1);
    usage.accounts.settle_at(NOW + 31, clock(NOW + 31), true);
    // An automatic check still waits for the regular interval.
    usage.accounts.settle_at(NOW + 4 * MINUTE, clock(NOW + 4 * MINUTE), false);
    assert_eq!(asked(), 2);

    provider.reply("deepseek", 429, "Retry-After: 1200\r\n", r#"{"error":{"message":"Rate limit reached"}}"#);
    let limited = NOW + 6 * MINUTE;
    usage.accounts.settle_at(limited, clock(limited), false);
    assert_eq!(asked(), 3);
    // The last answer still stands, and is not yet old enough to say so.
    let Status::Reading(reading, None) = status(&usage, deepseek, limited) else { panic!() };
    assert_eq!((reading.observed, reading.figure.text()), (NOW + 31, "¥110.00 balance".into()));

    provider.reply("deepseek", 200, "", DEEPSEEK);
    for (seconds, manual) in [(MINUTE, true), (10 * MINUTE, false), (19 * MINUTE, true)] {
        usage.accounts.settle_at(limited + seconds, clock(limited + seconds), manual);
        assert_eq!(asked(), 3, "asked {seconds}s into a 1200s wait");
    }
    let old = limited + 19 * MINUTE;
    let Status::Reading(_, Some(age)) = status(&usage, deepseek, old) else { panic!() };
    assert_eq!(age, old - (NOW + 31));
    usage.fixed_now = Some(old);
    assert_eq!(text(&mut usage, 60, 1), "DeepSeek · ¥110.00 balance · 24m ago");
    assert_eq!(text(&mut usage, 24, 1), "DeepSeek ¥110.00 last");
    assert_eq!(text(&mut usage, 15, 1), "DS ¥110.00 last");
    assert_eq!(text(&mut usage, 10, 1), "DS");

    let mut reopened = pane(&f, sources(&f, &provider, &[("MILO_DEEPSEEK_API_KEY", KEY)]), &[(deepseek, Show::Auto)]);
    reopened.accounts.settle_at(old, clock(old), true);
    assert_eq!(asked(), 3, "the saved rate limit also applies to a new pane");

    usage.accounts.settle_at(limited + HOUR, clock(limited + 20 * MINUTE - 1), true);
    assert_eq!(asked(), 3, "moving the wall clock forward does not shorten Retry-After");
    let now = limited - MINUTE;
    usage.accounts.settle_at(now, clock(limited + 20 * MINUTE), false);
    assert_eq!(asked(), 4, "moving the wall clock back does not extend Retry-After");
    assert!(matches!(status(&usage, deepseek, now), Status::Reading(_, None)));
}

#[test]
fn long_provider_deadlines_survive_manual_refresh_shared_cache_and_restart() {
    const RESTART: &str = "MILO_TEST_REOPEN_PROVIDER_PORT";
    if let Ok(port) = std::env::var(RESTART) {
        let sources = Sources::test(
            &[("MILO_DEEPSEEK_API_KEY", KEY)],
            "config/usage-accounts.conf".into(),
            "state/accounts".into(),
            port.parse().unwrap(),
        );
        let mut accounts = Accounts::with(sources, chosen(&[(UsageAccount::DeepSeek, Show::Auto)]));
        let now = NOW + 2 * HOUR - 1;
        accounts.settle(now, true);
        assert!(
            matches!(accounts.iter().next().unwrap().status(now), Status::Reading(reading, _) if reading.observed == NOW - MINUTE)
        );
        return;
    }
    for (code, header) in [
        (429, "Retry-After: 7200\r\n"),
        (403, "Retry-After: Thu, 19 Nov 2026 16:53:20 GMT\r\n"),
        (503, "Retry-After: 7200\r\n"),
    ] {
        let (f, provider) = (Fixture::folder(), Provider::start());
        let open = || {
            pane(&f, sources(&f, &provider, &[("MILO_DEEPSEEK_API_KEY", KEY)]), &[(UsageAccount::DeepSeek, Show::Auto)])
        };
        let (mut first, mut second) = (open(), open());
        let clock = Instant::now();
        provider.reply("deepseek", 200, "", DEEPSEEK);
        second.accounts.settle_at(NOW - MINUTE, clock, false);
        provider.reply("deepseek", code, header, "{}");
        first.accounts.settle_at(NOW, clock, true);
        assert_eq!(provider.requests(), 2);
        provider.reply("deepseek", 200, "", DEEPSEEK);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", std::thread::current().name().unwrap()])
            .env_clear()
            .env("HOME", &f.root)
            .env(RESTART, provider.port.to_string())
            .current_dir(&f.root)
            .output()
            .unwrap();
        assert!(
            child.status.success() && String::from_utf8_lossy(&child.stdout).contains("1 passed"),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        let mut reopened = open();
        reopened.accounts.settle_at(NOW - HOUR, clock, true);
        second.accounts.settle_at(NOW + HOUR, elapsed(clock, HOUR), true);
        for manual in [false, true] {
            first.accounts.settle_at(NOW + 4 * HOUR, elapsed(clock, 2 * HOUR - 1), manual);
            second.accounts.settle_at(NOW + 2 * HOUR - 1, elapsed(clock, 2 * HOUR - 1), manual);
            reopened.accounts.settle_at(NOW + 2 * HOUR - 1, elapsed(clock, 3 * HOUR - 1), manual);
            assert_eq!(provider.requests(), 2, "{code} {header}");
        }
        reopened.accounts.settle_at(NOW + 2 * HOUR, elapsed(clock, 3 * HOUR), true);
        assert_eq!(provider.requests(), 3);
    }
}

#[test]
fn provider_intervals_and_reload_floor_use_elapsed_time_in_an_open_pane() {
    for (manual, rest) in [(false, 5 * MINUTE), (true, 30)] {
        let (f, provider) = (Fixture::folder(), Provider::start());
        provider.reply("deepseek", 200, "", DEEPSEEK);
        let deepseek = UsageAccount::DeepSeek;
        let sources = sources(&f, &provider, &[("MILO_DEEPSEEK_API_KEY", KEY)]);
        let mut usage = pane(&f, sources, &[(deepseek, Show::Auto)]);
        let clock = Instant::now();
        usage.accounts.settle_at(NOW, clock, false);
        assert_eq!(provider.requests(), 1);

        usage.accounts.settle_at(NOW + HOUR, elapsed(clock, rest - 1), manual);
        assert_eq!(provider.requests(), 1, "a wall-clock advance must not shorten the wait (manual: {manual})");

        let now = NOW - MINUTE;
        usage.accounts.settle_at(now, elapsed(clock, rest), manual);
        assert_eq!(provider.requests(), 2, "a wall-clock rollback must not extend the wait (manual: {manual})");
        assert!(matches!(status(&usage, deepseek, now), Status::Reading(reading, None) if reading.observed == now));
    }
}

#[test]
fn a_refused_key_is_not_offered_again_for_half_an_hour_unless_asked() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    let openrouter = UsageAccount::OpenRouter;
    provider.reply("openrouter", 401, "", r#"{"error":{"code":401,"message":"No auth credentials found"}}"#);
    let mut usage =
        pane(&f, sources(&f, &provider, &[("MILO_OPENROUTER_API_KEY", KEY)]), &[(openrouter, Show::Always)]);
    let clock = Instant::now();
    usage.accounts.settle_at(NOW, clock, false);
    assert_eq!(status(&usage, openrouter, NOW), Status::Failed(Failure::Refused));
    assert_eq!(text(&mut usage, 60, 2), "OpenRouter · key was refused\nCheck the key");
    let rest = 29 * MINUTE;
    usage.accounts.settle_at(NOW + rest, elapsed(clock, rest), false);
    assert_eq!(provider.asked("openrouter").len(), 1);

    // Another pane opened meanwhile learns of the refusal from what was saved, without asking.
    let mut other =
        pane(&f, sources(&f, &provider, &[("MILO_OPENROUTER_API_KEY", KEY)]), &[(openrouter, Show::Always)]);
    other.accounts.settle(NOW + MINUTE, false);
    assert_eq!(status(&other, openrouter, NOW + MINUTE), Status::Failed(Failure::Refused));
    assert_eq!(provider.asked("openrouter").len(), 1);

    provider.reply("openrouter", 200, "", OPENROUTER);
    usage.accounts.settle_at(NOW + rest, elapsed(clock, rest), true);
    assert_eq!(provider.asked("openrouter").len(), 2);
    let Status::Reading(reading, None) = status(&usage, openrouter, NOW + rest) else { panic!() };
    assert_eq!(reading.figure, dollars(Kind::LimitLeft, 74.5));
    assert_eq!(reading.details, [Some(dollars(Kind::UsedToday, 1.25)), Some(dollars(Kind::UsedThisMonth, 25.5))]);
}

#[test]
fn panes_share_one_saved_answer_instead_of_each_asking_the_provider() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    provider.reply("deepseek", 200, "", DEEPSEEK);
    let deepseek = [(UsageAccount::DeepSeek, Show::Auto)];
    let open = |now| {
        let mut usage = pane(&f, sources(&f, &provider, &[("MILO_DEEPSEEK_API_KEY", KEY)]), &deepseek);
        usage.accounts.settle(now, false);
        status(&usage, UsageAccount::DeepSeek, now)
    };
    let first = open(NOW);
    assert_eq!(open(NOW + MINUTE), first);
    assert_eq!(open(NOW + 4 * MINUTE), first);
    assert_eq!(provider.requests(), 1);
    assert!(matches!(open(NOW + 5 * MINUTE), Status::Reading(reading, None) if reading.observed == NOW + 5 * MINUTE));
    assert_eq!(provider.requests(), 2);

    // An answer saved while the clock was ahead does not put off every later look.
    assert!(matches!(open(NOW - HOUR), Status::Reading(reading, None) if reading.observed == NOW - HOUR));
    assert_eq!(provider.requests(), 3);
}

#[test]
fn panes_and_doctor_keep_separate_readings_and_cooldowns_for_each_key() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    let router = UsageAccount::OpenRouter;
    let accounts = [(router, Show::Always)];
    let choices = chosen(&accounts);
    let key = |token| sources(&f, &provider, &[("MILO_OPENROUTER_API_KEY", token)]);
    let second = "sk-test-another-key";
    let refused = "sk-test-refused-key";
    let look = |token, now| {
        let mut usage = pane(&f, key(token), &accounts);
        usage.accounts.settle(now, false);
        status(&usage, router, now)
    };
    // This old format cannot tell which key was asked.
    f.write(
        "state/accounts/openrouter.json",
        &format!(r#"{{"version":1,"asked":{NOW},"next":{},"failure":"refused"}}"#, NOW + HOUR),
    );
    assert_eq!(key(KEY).doctor(choices, NOW)[1], "  no answer saved yet");
    provider.reply("openrouter", 200, "", OPENROUTER);
    let first = look(KEY, NOW);
    assert!(matches!(first, Status::Reading(reading, None) if reading.figure == dollars(Kind::LimitLeft, 74.5)));
    assert_eq!(key(second).doctor(choices, NOW)[1], "  no answer saved yet");

    provider.reply("openrouter", 401, "", "");
    assert_eq!(look(refused, NOW + 1), Status::Failed(Failure::Refused));
    assert_eq!(key(refused).doctor(choices, NOW + 1)[1], "  last attempt: the provider refused the key");

    provider.reply("openrouter", 200, "", &OPENROUTER.replace("74.5", "12.5"));
    let different = look(second, NOW + 2);
    assert!(matches!(different, Status::Reading(reading, None) if reading.figure == dollars(Kind::LimitLeft, 12.5)));
    assert_eq!(look(KEY, NOW + 3), first);
    assert_eq!(look(second, NOW + 3), different);
    assert_eq!(look(refused, NOW + 3), Status::Failed(Failure::Refused));
    assert!(key(KEY).doctor(choices, NOW + 3)[1].contains("$74.50"));
    assert!(key(second).doctor(choices, NOW + 3)[1].contains("$12.50"));
    assert_eq!(provider.requests(), 3);
}

#[test]
fn a_named_account_does_not_reuse_the_answer_for_another_scope_of_the_same_key() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    provider.reply("devin", 200, "", DEVIN);
    let look = |org, now| {
        let environment = [("MILO_DEVIN_API_KEY", KEY), ("MILO_DEVIN_ORG", org)];
        let mut usage = pane(&f, sources(&f, &provider, &environment), &[(UsageAccount::Devin, Show::Always)]);
        usage.accounts.settle(now, false);
        status(&usage, UsageAccount::Devin, now)
    };
    let first = look("org-abc", NOW);
    let second = look("org-def", NOW + 1);
    assert_ne!(first, second);
    assert_eq!(look("org-abc", NOW + 2), first);
    assert_eq!(look("org-def", NOW + 2), second);
    let requests = provider.asked("devin");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("/org-abc/") && requests[1].contains("/org-def/"));
}

#[test]
fn an_open_pane_drops_old_readings_and_in_flight_results_when_the_settings_key_changes() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    let router = UsageAccount::OpenRouter;
    let use_key = |key| {
        f.write("config/usage-accounts.conf", &format!("MILO_OPENROUTER_API_KEY={key}\n"));
        fs::set_permissions(f.root.join("config/usage-accounts.conf"), fs::Permissions::from_mode(0o600)).unwrap();
    };
    use_key(KEY);
    provider.reply("openrouter", 200, "", OPENROUTER);
    let mut usage = pane(&f, sources(&f, &provider, &[]), &[(router, Show::Always)]);
    usage.accounts.settle(NOW, false);
    assert!(text(&mut usage, 60, 1).contains("$74.50"));

    // Let an old-key reload finish without allowing the pane to collect it yet.
    usage.accounts.start(NOW + MINUTE, elapsed(Instant::now(), MINUTE), true);
    let mut fd = crate::sys::pollfd { fd: usage.external_fd().unwrap(), events: crate::sys::POLLIN, revents: 0 };
    // SAFETY: `fd` is writable and the pane keeps its job descriptor open.
    assert_eq!(unsafe { libc::poll(&raw mut fd, 1, 5000) }, 1);
    use_key("sk-test-replacement-key");
    assert!(matches!(usage.check_external(), External::Redraw));
    assert_eq!(status(&usage, router, NOW + MINUTE), Status::Checking);
    assert!(!text(&mut usage, 60, 1).contains("$74.50"));

    provider.reply("openrouter", 200, "", &OPENROUTER.replace("74.5", "12.5"));
    usage.accounts.settle(NOW + MINUTE + 1, false);
    assert!(text(&mut usage, 60, 1).contains("$12.50"));
    use_key(KEY);
    usage.accounts.start(NOW + MINUTE + 2, Instant::now(), false);
    assert!(!text(&mut usage, 60, 1).contains("$12.50"));
    usage.accounts.settle(NOW + MINUTE + 2, false);
    assert!(text(&mut usage, 60, 1).contains("$74.50"));
    assert_eq!(provider.requests(), 3);
}

#[test]
fn a_reply_in_another_form_is_unknown_and_never_a_figure_of_zero() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    let environment = [
        ("MILO_DEEPSEEK_API_KEY", KEY),
        ("MILO_OPENROUTER_API_KEY", KEY),
        ("MILO_DEVIN_API_KEY", KEY),
        ("MILO_DEVIN_ORG", "org-abc"),
        ("MILO_GITHUB_TOKEN", KEY),
        ("MILO_GITHUB_USER", "example-user"),
    ];
    let mut round = 0;
    let mut look = |which: UsageAccount, status_code: u16, header: &'static str, body: &str| {
        // Each look starts without a saved answer.
        round += 1;
        let state = format!("state-{round}");
        provider.reply(which.id(), status_code, header, body);
        let sources =
            Sources::test(&environment, f.root.join("none.conf"), f.root.join(state).join("accounts"), provider.port);
        let mut usage = pane(&f, sources, &[(which, Show::Always)]);
        usage.accounts.settle(NOW, false);
        status(&usage, which, NOW)
    };
    let failed = |failure| Status::Failed(failure);
    use UsageAccount::{Copilot, DeepSeek, Devin, OpenRouter};

    for body in [
        "",
        "<html>Sign in</html>",
        "{}",
        r#"{"balance_infos":[]}"#,
        r#"{"balance_infos":[{"currency":"EUR","total_balance":"1.00"}]}"#,
        r#"{"balance_infos":[{"currency":"USD","total_balance":"-1"}]}"#,
        r#"{"balance_infos":[{"currency":"USD","total_balance":"lots"}]}"#,
    ] {
        assert_eq!(look(DeepSeek, 200, "", body), failed(Failure::Unexpected), "{body}");
    }
    let low = r#"{"is_available":false,"balance_infos":[{"currency":"USD","total_balance":"0.00","granted_balance":"0.00"},{"currency":"CNY","total_balance":"3.50"}]}"#;
    let Status::Reading(reading, None) = look(DeepSeek, 200, "", low) else { panic!() };
    assert!(reading.depleted && reading.figure == dollars(Kind::Balance, 0.0));
    let other = Figure { kind: Kind::Balance, unit: Unit::Money(Currency::Cny), value: 3.5 };
    assert_eq!(reading.details, [None, Some(other)]);

    // A key without a limit reports what it has used; an overspent limit has nothing left, not less.
    let unlimited =
        r#"{"data":{"limit":null,"limit_remaining":null,"usage":3.25,"usage_daily":0,"usage_monthly":3.25}}"#;
    let Status::Reading(reading, None) = look(OpenRouter, 200, "", unlimited) else { panic!() };
    assert_eq!(
        (reading.figure, reading.details),
        (dollars(Kind::Used, 3.25), [None, Some(dollars(Kind::UsedThisMonth, 3.25))])
    );
    let overspent = r#"{"data":{"limit":5,"limit_remaining":-0.25,"usage":5.25}}"#;
    let Status::Reading(reading, None) = look(OpenRouter, 200, "", overspent) else { panic!() };
    assert_eq!(reading.figure, dollars(Kind::LimitLeft, 0.0));
    // An amount the provider rounded to nothing from below is nothing, not a negative figure.
    let rounded = r#"{"data":{"limit":5,"limit_remaining":-0.0,"usage":5}}"#;
    let Status::Reading(reading, None) = look(OpenRouter, 200, "", rounded) else { panic!() };
    assert_eq!(reading.figure.text(), "$0.00 key limit left");
    let owed = r#"{"is_available":false,"balance_infos":[{"currency":"CNY","total_balance":"-0.00"}]}"#;
    let Status::Reading(reading, None) = look(DeepSeek, 200, "", owed) else { panic!() };
    assert_eq!(reading.figure.text(), "¥0.00 balance");
    assert_eq!(
        look(OpenRouter, 200, "", r#"{"data":{"limit_remaining":"plenty","usage":1}}"#),
        failed(Failure::Unexpected)
    );
    assert_eq!(look(OpenRouter, 200, "", r#"{"limit_remaining":1,"usage":1}"#), failed(Failure::Unexpected));

    // Devin answers plans without the report with 403; a day with nothing used is a reported zero.
    assert_eq!(look(Devin, 403, "", r#"{"title":"Forbidden","status":403}"#), failed(Failure::Denied));
    let Status::Reading(reading, None) = look(Devin, 200, "", r#"{"total_acus":0,"consumption_by_date":[]}"#) else {
        panic!()
    };
    assert_eq!(reading.figure.text(), "0 ACUs today");
    assert_eq!(look(Devin, 200, "", r#"{"consumption_by_date":[]}"#), failed(Failure::Unexpected));

    // GitHub returns an empty report for an account billed through an organization.
    let empty = r#"{"timePeriod":{"year":2026,"month":11},"user":"example-user","usageItems":[]}"#;
    assert_eq!(look(Copilot, 200, "", empty), failed(Failure::Nothing));
    let mixed = COPILOT.replacen("\"ai-credits\"", "\"requests\"", 1);
    assert_eq!(look(Copilot, 200, "", &mixed), failed(Failure::Unexpected));
    let last_month = COPILOT.replace("\"month\": 11", "\"month\": 10");
    assert_eq!(look(Copilot, 200, "", &last_month), failed(Failure::Unexpected));
    // Quantities too large to add into a count, or a year no calendar holds, are no report.
    let endless = COPILOT.replace("1100.5", "1e308").replace("139.5", "1e308");
    assert_eq!(look(Copilot, 200, "", &endless), failed(Failure::Unexpected));
    let no_such_year = COPILOT.replace("\"year\": 2026", "\"year\": 9223372036854775807");
    assert_eq!(look(Copilot, 200, "", &no_such_year), failed(Failure::Unexpected));
    assert_eq!(look(Copilot, 404, "", r#"{"message":"Not Found"}"#), failed(Failure::Denied));
    assert_eq!(
        look(Copilot, 403, "Retry-After: 60\r\n", r#"{"message":"secondary rate limit"}"#),
        failed(Failure::Limited)
    );
    assert_eq!(look(Copilot, 503, "", ""), failed(Failure::Unreachable));
    assert_eq!(look(Copilot, 302, "Location: https://example.com/elsewhere\r\n", ""), failed(Failure::Unreachable));
}

#[test]
fn a_count_is_asked_for_again_when_its_period_ends_and_not_a_whole_interval_later() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    provider.reply("devin", 200, "", DEVIN);
    let devin = UsageAccount::Devin;
    let environment = [("MILO_DEVIN_API_KEY", KEY), ("MILO_DEVIN_ORG", "org-abc")];
    let mut usage = pane(&f, sources(&f, &provider, &environment), &[(devin, Show::Auto)]);
    let (_, end) = DEVIN_DAY;
    // Asked two minutes before Devin's day ends, the schedule alone would wait
    // thirteen minutes into the next day with no figure to show.
    let clock = Instant::now();
    usage.accounts.settle_at(end - 2 * MINUTE, clock, false);
    usage.accounts.settle_at(end - 1, elapsed(clock, 2 * MINUTE - 1), false);
    assert_eq!(provider.asked("devin").len(), 1);
    assert!(matches!(status(&usage, devin, end - 1), Status::Reading(reading, None) if reading.until == Some(end)));

    usage.accounts.settle_at(end, elapsed(clock, 2 * MINUTE), false);
    let asked = provider.asked("devin");
    assert_eq!(asked.len(), 2);
    assert!(asked[1].contains(&format!("time_after={end}&time_before={} ", end + DAY)), "{asked:?}");
    assert!(matches!(status(&usage, devin, end), Status::Reading(reading, None) if reading.until == Some(end + DAY)));
}

#[test]
fn unreachable_attempts_retry_after_one_minute_and_keep_the_last_reading() {
    for failure in [0, 503] {
        let (f, provider) = (Fixture::folder(), Provider::start());
        let devin = UsageAccount::Devin;
        let environment = [("MILO_DEVIN_API_KEY", KEY), ("MILO_DEVIN_ORG", "org-abc")];
        let sources = sources(&f, &provider, &environment);
        let choices = [(devin, Show::Always)];
        let mut usage = pane(&f, sources.clone(), &choices);
        provider.reply("devin", 200, "", DEVIN);
        let start = Instant::now();
        let clock = |now| elapsed(start, now - NOW);
        usage.accounts.settle_at(NOW, clock(NOW), false);

        let wake = NOW + 20 * MINUTE;
        provider.reply("devin", failure, "", "");
        usage.accounts.settle_at(wake, clock(wake), false);
        let reading = status(&usage, devin, wake);
        assert!(matches!(reading, Status::Reading(reading, None) if reading.observed == NOW));
        assert_eq!(provider.requests(), 2);

        provider.reply("devin", 200, "", DEVIN);
        usage.accounts.settle_at(wake + MINUTE - 1, clock(wake + MINUTE - 1), false);
        let mut reopened = pane(&f, sources, &choices);
        reopened.accounts.settle_at(wake + MINUTE - 1, clock(wake + MINUTE - 1), false);
        assert_eq!(status(&reopened, devin, wake + MINUTE - 1), reading);
        assert_eq!(provider.requests(), 2);

        reopened.accounts.settle_at(wake + MINUTE, clock(wake + MINUTE), false);
        assert!(matches!(status(&reopened, devin, wake + MINUTE), Status::Reading(reading, None)
            if reading.observed == wake + MINUTE));
        usage.accounts.settle_at(wake + MINUTE, clock(wake + MINUTE), false);
        assert_eq!(status(&usage, devin, wake + MINUTE), status(&reopened, devin, wake + MINUTE));
        assert_eq!(provider.requests(), 3);
    }
}

#[test]
fn a_missing_curl_is_told_apart_from_a_provider_that_does_not_answer() {
    let f = Fixture::folder();
    let me = std::env::current_exe().unwrap();
    assert_eq!(Failure::without_reply(f.root.join("no-such-curl").as_os_str()), Failure::NoCurl);
    assert_eq!(Failure::without_reply(me.as_os_str()), Failure::Unreachable);

    let devin = UsageAccount::Devin;
    let sources = Sources::test(&[], f.root.join("none.conf"), f.root.join("state/accounts"), 9);
    let mut usage = pane(&f, sources, &[(devin, Show::Always)]);
    usage.accounts.set(devin, None, Some(Failure::NoCurl), NOW);
    assert_eq!(text(&mut usage, 60, 2), "Devin · curl not installed\nInstall curl");
}

#[test]
fn an_unreachable_provider_keeps_the_last_answer_until_it_is_a_day_old_or_its_period_ends() {
    let f = Fixture::folder();
    // A port nothing listens on: the listener is closed before any look.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let devin = UsageAccount::Devin;
    let environment = [("MILO_DEVIN_API_KEY", KEY), ("MILO_DEVIN_ORG", "org-abc")];
    let sources = Sources::test(&environment, f.root.join("none.conf"), f.root.join("state/accounts"), port);
    let mut usage = pane(&f, sources, &[(devin, Show::Always)]);
    usage.accounts.settle(NOW, false);
    assert_eq!(status(&usage, devin, NOW), Status::Failed(Failure::Unreachable));
    assert_eq!(text(&mut usage, 60, 2), "Devin · could not reach provider\nPress r to try again");

    let figure = Figure { kind: Kind::UsedToday, unit: Unit::Acus, value: 12.5 };
    let reading = Reading { observed: NOW, figure, details: [None, None], until: Some(DEVIN_DAY.1), depleted: false };
    usage.accounts.set(devin, Some(reading), Some(Failure::Unreachable), NOW);
    assert_eq!(status(&usage, devin, NOW + 31 * MINUTE), Status::Reading(reading, None));
    assert_eq!(status(&usage, devin, NOW + 32 * MINUTE), Status::Reading(reading, Some(32 * MINUTE)));
    // Yesterday's count is not today's, however recently it was read.
    assert_eq!(status(&usage, devin, DEVIN_DAY.1 - 1), Status::Reading(reading, Some(DEVIN_DAY.1 - 1 - NOW)));
    assert_eq!(status(&usage, devin, DEVIN_DAY.1), Status::Failed(Failure::Unreachable));

    let deepseek = UsageAccount::DeepSeek;
    let mut usage =
        pane(&f, Sources::test(&[], f.root.join("none.conf"), f.root.join("s"), port), &[(deepseek, Show::Auto)]);
    usage.accounts.set(deepseek, Some(balance(NOW)), None, NOW);
    assert!(matches!(status(&usage, deepseek, NOW + DAY), Status::Reading(_, Some(DAY))));
    assert_eq!(status(&usage, deepseek, NOW + DAY + 1), Status::Outdated);
    usage.fixed_now = Some(NOW + DAY + 1);
    assert_eq!(text(&mut usage, 60, 3), "No usage to show yet\nDeepSeek · no recent answer\nPress r to ask again");
    // A reading dated ahead of the clock is not a reading either.
    assert_eq!(status(&usage, deepseek, NOW - 6 * MINUTE), Status::Outdated);
}

/// Codex and Claude Code limits on disk, with DeepSeek and OpenRouter answered.
fn with_tools(f: &Fixture, codex: bool, claude: bool) -> Usage {
    if codex {
        let window = codex_window(27, 300, NOW + 2 * HOUR);
        write(f, CODEX, &codex_snapshot(&format!("{}Z", iso(NOW)), &window, "null"), NOW);
    }
    if claude {
        let saved =
            format!(r#"{{"rate_limits":{{"five_hour":{{"used_percentage":52,"resets_at":{}}}}}}}"#, NOW + 2 * HOUR);
        assert_eq!(status::run(saved.as_bytes(), Vec::new(), Some(&f.root.join(STATE).join(SAVED_LIMITS)), NOW), 0);
    }
    let mut usage = automatic(f, NOW);
    usage.check();
    let both = [(UsageAccount::DeepSeek, Show::Auto), (UsageAccount::OpenRouter, Show::Auto)];
    usage.accounts = Accounts::with(Sources::test(&[], f.root.join("none.conf"), f.root.join("s"), 1), chosen(&both));
    usage.accounts.set(UsageAccount::DeepSeek, Some(balance(NOW)), None, NOW);
    let spend = Reading { figure: dollars(Kind::Used, 25.5), ..balance(NOW) };
    usage.accounts.set(UsageAccount::OpenRouter, Some(spend), None, NOW);
    usage
}

#[test]
fn accounts_take_rows_beneath_the_tools_only_once_every_limit_has_its_own() {
    let f = Fixture::folder();
    let mut usage = with_tools(&f, true, true);
    // The default rail: two rows, two limits, and no room taken from either.
    let rail = text(&mut usage, 40, 2);
    assert_eq!(rail, "Codex 5h · 73% left · resets 2h\nClaude 5h · 48% left · resets 2h");
    // Each further row goes to an account, in order.
    let three = text(&mut usage, 40, 3);
    assert_eq!(three, format!("{rail}\nDeepSeek · ¥110.00 balance"));
    assert_eq!(text(&mut usage, 40, 4), format!("{three}\nOpenRouter · $25.50 used"));
    // A wide rail has room for both limits on one row, so an account shares even two rows.
    let wide = text(&mut usage, 80, 2);
    assert!(wide.starts_with("Codex 5h · 73% left") && wide.contains("│") && wide.contains("Claude 5h · 48% left"));
    assert!(wide.ends_with("\nDeepSeek · ¥110.00 balance"), "{wide}");

    // Wide panels keep three rows; the accounts sit at the foot, above the key hints.
    let panels = frame(&mut usage, 100, 12);
    assert!(
        panels[9].trim() == "DeepSeek · ¥110.00 balance" && panels[10].trim() == "OpenRouter · $25.50 used",
        "{panels:#?}"
    );
    assert!(panels[11].contains("r reload"));
    let panel = panels[..9].join("\n");
    assert!(panel.contains("Codex") && panel.contains("Claude") && panel.contains("resets in 2 h"), "{panel}");

    let f = Fixture::folder();
    let mut usage = with_tools(&f, true, false);
    assert_eq!(text(&mut usage, 40, 2), "Codex 5h · 73% left · resets 2h\nDeepSeek · ¥110.00 balance");
    assert_eq!(text(&mut usage, 40, 1), "Codex 5h · 73% left · resets 2h");
}

#[test]
fn no_account_figure_is_ever_drawn_or_streamed_as_a_percentage_or_a_limit_window() {
    let f = Fixture::folder();
    let mut usage = with_tools(&f, false, false);
    usage.accounts.set(
        UsageAccount::OpenRouter,
        Some(Reading { figure: dollars(Kind::LimitLeft, 74.5), ..balance(NOW) }),
        None,
        NOW,
    );
    assert_eq!(text(&mut usage, 60, 2), "DeepSeek · ¥110.00 balance\nOpenRouter · $74.50 key limit left");
    for large in [false, true] {
        usage.large_figures = large;
        for width in [1, 6, 8, 12, 17, 22, 28, 30, 36, 48, 64, 100, 140] {
            for height in [1, 2, 3, 4, 5, 8, 9, 12, 16, 24] {
                let rows = frame(&mut usage, width, height).join("\n");
                assert!(!rows.contains('%') && !rows.contains("█"), "{width}x{height}:\n{rows}");
                assert!(width < 18 || rows.contains("¥110.00"), "{width}x{height}:\n{rows}");
            }
        }
    }
    assert_eq!(text(&mut usage, 14, 2), "DS ¥110.00\nOR $74.50");

    let mut line = String::new();
    usage.menu_snapshot(&mut line, crate::menubar::Indicator::Quota, false);
    let snapshot = Value::parse(line.as_bytes()).unwrap();
    let [providers] = snapshot.fields(["providers"]);
    let providers: Vec<_> = providers.unwrap().elements().unwrap().collect();
    assert_eq!(providers.len(), 2);
    for (provider, (id, detail)) in
        providers.into_iter().zip([("deepseek", "¥110.00 balance"), ("openrouter", "$74.50 key limit left")])
    {
        let [name, windows, said] = provider.fields(["id", "windows", "detail"]);
        assert!(name.unwrap().is(id) && said.unwrap().is(detail), "{line}");
        // The badge and its alerts read windows only, so an account can never drive them.
        assert_eq!(windows.unwrap().elements().unwrap().count(), 0);
    }

    // Without a figure the menu says why only when the account is always shown.
    usage.accounts = Accounts::with(
        Sources::test(&[], f.root.join("none.conf"), f.root.join("s"), 1),
        chosen(&[(UsageAccount::Devin, Show::Auto), (UsageAccount::Copilot, Show::Always)]),
    );
    usage.accounts.set(UsageAccount::Devin, None, Some(Failure::Denied), NOW);
    usage.accounts.set(UsageAccount::Copilot, None, Some(Failure::Limited), NOW);
    line.clear();
    usage.menu_snapshot(&mut line, crate::menubar::Indicator::Quota, false);
    assert_eq!(
        line,
        "{\"version\":1,\"indicator\":\"quota\",\"attention\":\"off\",\"attentionStates\":\"waiting,errors\",\"providers\":[{\"id\":\"copilot\",\"name\":\"Copilot\",\"windows\":[],\"detail\":\"Rate limited\"}]}\n"
    );
}

#[test]
fn doctor_describes_an_account_from_its_settings_and_saved_answer_without_asking() {
    let (f, provider) = (Fixture::folder(), Provider::start());
    provider.reply("deepseek", 200, "", DEEPSEEK);
    let on = chosen(&[(UsageAccount::DeepSeek, Show::Auto), (UsageAccount::Devin, Show::Always)]);
    let sources = sources(&f, &provider, &[("MILO_DEEPSEEK_API_KEY", KEY), ("MILO_DEVIN_API_KEY", KEY)]);
    let file = f.root.join("config/usage-accounts.conf");
    assert_eq!(
        sources.doctor(on, NOW),
        [
            "DeepSeek balance: asks api.deepseek.com with MILO_DEEPSEEK_API_KEY from the environment, usually every 5 minutes"
                .to_owned(),
            "  no answer saved yet".to_owned(),
            format!("Devin ACUs: not asked; set MILO_DEVIN_ORG in the environment or in {}", file.display()),
        ]
    );
    assert_eq!(provider.requests(), 0);

    let mut usage = pane(&f, sources.clone(), &[(UsageAccount::DeepSeek, Show::Auto)]);
    usage.accounts.settle(NOW, false);
    let report = sources.doctor(on, NOW + 3 * MINUTE);
    assert_eq!(report[1], "  last answer: ¥110.00 balance, 3m ago");
    assert!(report.iter().all(|line| !line.contains(KEY)), "{report:?}");
    assert_eq!(provider.requests(), 1);
    assert_eq!(sources.doctor(UsageAccounts::default(), NOW), [""; 0]);
}

/// One real scheduler check and, if due, its complete worker batch. Passing
/// the artificial clock to collection avoids mixing real and simulated ticks.
fn schedule_look(accounts: &mut Accounts, now: i64, clock: Instant, manual: bool) -> (Duration, Option<Duration>) {
    let start = Instant::now();
    accounts.start(now, clock, manual);
    let inline = start.elapsed();
    if accounts.fd().is_none() {
        return (inline, None);
    }
    while let Some(fd) = accounts.fd() {
        ready(fd);
        accounts.collect(clock);
    }
    (inline, Some(start.elapsed()))
}

#[test]
#[ignore = "a benchmark; run it alone on a release build"]
fn usage_profile_account_schedule() {
    if !super::super::http::executable().is_file() {
        eprintln!("skipping account fixture: curl is not installed");
        return;
    }
    const SECONDS: i64 = 40 * MINUTE;
    const STEP: i64 = 2;
    let environment = [
        ("MILO_DEEPSEEK_API_KEY", KEY),
        ("MILO_OPENROUTER_API_KEY", "sk-or-test-key-1"),
        ("MILO_DEVIN_API_KEY", "cog_test_key_22"),
        ("MILO_DEVIN_ORG", "org-abc123def456"),
        ("MILO_GITHUB_TOKEN", "github_pat_test_333"),
        ("MILO_GITHUB_USER", "example-user"),
    ];
    let all = UsageAccount::ALL.map(|which| (which, Show::Always));
    println!("# 40 simulated minutes, two seconds between looks; four canned loopback providers");
    println!(
        "settings\tphase\tlooks\tmanual\tworker_ticks\trequests\tno_job_p50_us\tno_job_p95_us\tlaunch_p50_us\tlaunch_p95_us\tresult_p50_us\tresult_p95_us"
    );
    for (mode, enabled, file) in [("hidden", false, false), ("environment", true, false), ("private-file", true, true)]
    {
        let (f, provider) = (Fixture::folder(), Provider::start());
        for (id, body) in [("deepseek", DEEPSEEK), ("openrouter", OPENROUTER), ("devin", DEVIN), ("copilot", COPILOT)] {
            provider.reply(id, 200, "", body);
        }
        if file {
            let mut text = "# Synthetic usage account settings, no live tokens.\n".to_owned();
            for (key, value) in environment {
                text.push_str(&format!("{key} = {value}\n"));
            }
            f.write("config/usage-accounts.conf", &text);
            fs::set_permissions(f.root.join("config/usage-accounts.conf"), fs::Permissions::from_mode(0o600)).unwrap();
        }
        let (keys, chosen) = if enabled && !file {
            (environment.as_slice(), all.as_slice())
        } else if enabled {
            (&[][..], all.as_slice())
        } else {
            (environment.as_slice(), &[][..])
        };
        let mut usage = pane(&f, sources(&f, &provider, keys), chosen);
        let clock = Instant::now();
        let (mut no_job, mut launch, mut result) = (Vec::new(), Vec::new(), Vec::new());
        let mut reopen = None;
        for tick in 0..=SECONDS / STEP {
            let passed = tick * STEP;
            let now = NOW + passed;
            let at = elapsed(clock, passed);
            if passed == 5 * MINUTE {
                provider.reply("deepseek", 429, "Retry-After: 1200\r\n", "{}");
            }
            let manual = matches!(passed, 10 | 600 | 610);
            let (inline, complete) = schedule_look(&mut usage.accounts, now, at, manual);
            if let Some(complete) = complete {
                launch.push(inline);
                result.push(complete);
            } else {
                no_job.push(inline);
            }
            if passed == 5 * MINUTE {
                provider.reply("deepseek", 200, "", DEEPSEEK);
            }
            if enabled && passed == 5 * MINUTE + STEP {
                let mut second = pane(&f, sources(&f, &provider, keys), chosen);
                let previous = provider.requests();
                let (inline, full) = schedule_look(&mut second.accounts, now, at, false);
                assert_eq!(provider.requests(), previous, "fresh saved answers and Retry-After must survive reopening");
                for &(which, _) in chosen {
                    assert_eq!(status(&second, which, now), status(&usage, which, now));
                }
                reopen = Some((inline, full.expect("reopening loads the saved answers")));
            }
        }
        for (id, due) in [("deepseek", 6), ("openrouter", 9), ("devin", 4), ("copilot", 4)] {
            assert_eq!(provider.asked(id).len(), if enabled { due } else { 0 }, "{id}");
        }
        let looks = usize::try_from(SECONDS / STEP + 1).unwrap();
        let batches = result.len();
        println!(
            "{mode}\topen\t{looks}\t3\t{batches}\t{}\t{}\t{}\t{}",
            provider.requests(),
            micros(&mut no_job),
            micros(&mut launch),
            micros(&mut result)
        );
        if let Some((inline, complete)) = reopen {
            println!("{mode}\treopen\t1\t0\t1\t0\t-\t-\t{}\t{}", micros(&mut [inline]), micros(&mut [complete]));
        }
    }
}
