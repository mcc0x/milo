//! Each local source failing in the ways a provider's files fail: missing,
//! rotated, locked, stale, or written in a shape Milo does not know.
//!
//! One rule holds in every case. The pane shows only a figure that a source
//! reported, and says so once that figure is old. A source that cannot be read
//! is unknown; it is never zero, never a fresh 100% and never another number.
//! A new source can reuse `lock_out`, `replace` and `look` for its own files.

use std::os::unix::fs::PermissionsExt as _;
use std::process::{Child, Command, Stdio};

use super::exec;
use super::omp_limits::{report, sqlite, wait_for, wait_omp, window};
use super::*;

/// What `healthy` shows: the tightest window of each tool, read a minute ago.
const HEALTHY: [&str; 2] = ["Codex week · 60% left", "Claude week · 39% left"];
const UNKNOWN: [&str; 2] = ["Codex · no sessions", "Claude no saved limits"];
const CLAUDE_CONFIG: &str = ".claude.json";

/// A Codex session log and a Claude status save, each a minute old.
fn healthy() -> Fixture {
    let f = Fixture::folder();
    write(&f, CODEX, &codex(NOW - MINUTE, 40, NOW + DAY, "null"), NOW - MINUTE);
    assert_eq!(status::run(STATUS, Vec::new(), Some(&save(&f)), NOW - MINUTE), 0);
    f
}

fn save(f: &Fixture) -> PathBuf {
    f.root.join(STATE).join(SAVED_LIMITS)
}

/// Claude Code's own cache of one weekly window, for the signed-in account.
fn claude_cache(at: i64, account: &str, week: &str) -> String {
    format!(
        r#"{{"oauthAccount":{{"accountUuid":"fixture-current"}},"cachedUsageUtilization":{{"fetchedAtMs":{},
            "accountUuid":"{account}","utilization":{{"five_hour":null,"seven_day":{week}}}}}}}"#,
        at * 1000
    )
}

/// The pane at `NOW`, also reading Claude Code's own cache.
fn pane(f: &Fixture) -> Usage {
    let mut usage = unopened(f, NOW);
    usage.claude_config = Some(f.root.join(CLAUDE_CONFIG));
    usage
}

/// Reloads as `r` does and returns the two-row pane: one line for each tool.
fn look(usage: &mut Usage) -> [String; 2] {
    assert_eq!(usage.key(Key::Char('r')), After::Stay);
    if usage.external_fd().is_some() {
        wait_omp(usage);
    }
    frame(usage, 22, 2).try_into().unwrap()
}

fn sources(usage: &Usage) -> [Option<&'static str>; 2] {
    [
        usage.codex.reported(usage.now()).map(|(_, source)| source.label()),
        usage.claude.reported(usage.now()).map(|(_, source)| source.label()),
    ]
}

/// Puts new content where `path` is by renaming, as a rotation or an atomic save does.
fn replace(f: &Fixture, path: &str, data: &str, now: i64) {
    write(f, "replacement", data, now);
    fs::rename(f.root.join("replacement"), f.root.join(path)).unwrap();
}

/// A path nobody may read until this is dropped.
struct NoAccess {
    path: PathBuf,
    mode: u32,
}

fn lock_out(path: PathBuf) -> NoAccess {
    let mode = fs::metadata(&path).unwrap().permissions().mode();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    NoAccess { path, mode }
}

impl Drop for NoAccess {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode)).unwrap();
    }
}

/// Permission bits do not stop root, so the permission cases mean nothing for it.
fn root() -> bool {
    // SAFETY: Asking for the effective user ID touches no memory.
    unsafe { libc::geteuid() == 0 }
}

/// A database holding one weekly report for each tool, fetched at `NOW`.
fn omp_database(f: &Fixture, codex_used: &str, claude_used: &str) -> Option<PathBuf> {
    let db = sqlite(f)?;
    let reset = (NOW + DAY) * 1000;
    let limit = |id, used| format!("[{}]", window(id, used, reset, 10_080, "null"));
    report(&db, "codex", "openai-codex", NOW * 1000, &limit("openai-codex:primary", codex_used));
    report(&db, "claude", "anthropic", NOW * 1000, &limit("anthropic:7d", claude_used));
    Some(db)
}

fn omp_pane(f: &Fixture, db: &Path) -> Usage {
    let mut usage = unopened(f, NOW);
    usage.omp = omp::poll(Some(db.to_owned()), TEST_WAIT);
    usage
}

/// Another program in the middle of writing the database, which shuts readers out.
struct Writer(Child);

fn writing(f: &Fixture, db: &Path) -> Writer {
    let mut writer = Command::new(sqlite::executable())
        .args(["-batch", "-init", "/dev/null"])
        .current_dir(&f.root)
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    writeln!(writer.stdin.as_mut().unwrap(), "BEGIN EXCLUSIVE;\n.once writing\nSELECT 1;").unwrap();
    wait_for(|| f.root.join("writing").exists());
    Writer(writer)
}

impl Drop for Writer {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}

#[test]
fn a_missing_source_is_unknown_and_takes_its_figure_with_it() {
    let f = healthy();
    write(&f, CLAUDE_CONFIG, &claude_cache(NOW - 2 * MINUTE, "fixture-current", r#"{"utilization":18}"#), NOW);
    let mut usage = pane(&f);
    assert_eq!(look(&mut usage), HEALTHY);

    // With the newest report gone, the one beneath it shows through.
    fs::remove_file(save(&f)).unwrap();
    assert_eq!(look(&mut usage), ["Codex week · 60% left", "Claude week · 82% left"]);
    assert_eq!(sources(&usage), [Some("Codex session log"), Some("Claude Code usage cache")]);

    fs::remove_file(f.root.join(CLAUDE_CONFIG)).unwrap();
    fs::remove_file(f.root.join(CODEX)).unwrap();
    assert_eq!(look(&mut usage), UNKNOWN);
    fs::remove_dir_all(f.root.join("codex")).unwrap();
    fs::remove_dir_all(f.root.join(STATE)).unwrap();
    assert_eq!(look(&mut usage), UNKNOWN);
    let rows = frame(&mut usage, 140, 4);
    assert!(rows[1].matches('—').count() == 2 && !rows[1].contains('%'), "{rows:?}");
    assert!(rows[2].contains("no sessions found · Run Codex"), "{rows:?}");
    assert!(rows[2].contains("no saved limits found · Run milo setup"), "{rows:?}");

    let f = Fixture::folder();
    let Some(db) = omp_database(&f, "0.25", "0.27") else { return };
    let mut usage = omp_pane(&f, &db);
    assert_eq!(look(&mut usage), ["Codex week · 75% left", "Claude week · 73% left"]);
    fs::remove_file(db).unwrap();
    assert_eq!(look(&mut usage), UNKNOWN);
}

#[test]
fn a_rotated_source_is_read_afresh_from_the_file_now_at_its_path() {
    let f = healthy();
    let mut usage = pane(&f);
    assert_eq!(look(&mut usage), HEALTHY);

    // The log moves aside and a longer one takes its name. Its only report
    // lies before the place where reading the old file stopped.
    let read = fs::metadata(f.root.join(CODEX)).unwrap().len();
    fs::rename(f.root.join(CODEX), f.root.join(format!("{CODEX}.1"))).unwrap();
    let padding = "{}\n".repeat(usize::try_from(read).unwrap());
    replace(&f, CODEX, &format!("{}{padding}", codex(NOW - 30, 64, NOW + DAY, "null")), NOW - 30);
    assert_eq!(look(&mut usage)[0], "Codex week · 36% left");

    // A new day starts a new log. Compressing the old one changes nothing.
    let today = "codex/2026/11/19/rollout-b.jsonl";
    write(&f, today, &codex(NOW, 82, NOW + DAY, "null"), NOW);
    assert_eq!(look(&mut usage)[0], "Codex week · 18% left");
    fs::rename(f.root.join(CODEX), f.root.join(format!("{CODEX}.zst"))).unwrap();
    assert_eq!(look(&mut usage)[0], "Codex week · 18% left");
    // A log that starts over has no report yet, and the report it replaced is gone.
    replace(&f, today, "{}\n", NOW);
    assert_eq!(look(&mut usage)[0], "Codex · no limits");

    // Putting back an older save shows that save, at its own age.
    let then = NOW - 2 * DAY;
    let older = format!(
        "{{\"saved_at\":{then},\"rate_limits\":{{\"five_hour\":null,\
         \"seven_day\":{{\"used_percentage\":20,\"observed_at\":{then}}}}}}}\n"
    );
    replace(&f, &format!("{STATE}/{SAVED_LIMITS}"), &older, then);
    assert_eq!(look(&mut usage)[1], "Claude week · 80% last");

    // Claude Code replaces its settings file as it saves. A cache left there
    // for another account is not this account's reading.
    replace(&f, CLAUDE_CONFIG, &claude_cache(NOW, "fixture-current", r#"{"utilization":18}"#), NOW);
    assert_eq!(look(&mut usage)[1], "Claude week · 82% left");
    replace(&f, CLAUDE_CONFIG, &claude_cache(NOW, "fixture-other", r#"{"utilization":5}"#), NOW);
    assert_eq!(look(&mut usage)[1], "Claude week · 80% last");

    let f = Fixture::folder();
    let Some(db) = omp_database(&f, "0.25", "0.27") else { return };
    let mut usage = omp_pane(&f, &db);
    assert_eq!(look(&mut usage), ["Codex week · 75% left", "Claude week · 73% left"]);
    fs::rename(&db, f.root.join("agent.db.1")).unwrap();
    omp_database(&f, "0.5", "0.6").unwrap();
    assert_eq!(look(&mut usage), ["Codex week · 50% left", "Claude week · 40% left"]);
}

#[test]
fn a_locked_source_is_unknown_until_it_can_be_read_again() {
    if root() {
        eprintln!("skipping permission fixtures: root can read every file");
        return;
    }
    let f = healthy();
    write(&f, CLAUDE_CONFIG, &claude_cache(NOW - 2 * MINUTE, "fixture-current", r#"{"utilization":18}"#), NOW);
    let mut usage = pane(&f);

    // Sessions that exist but cannot be opened are not "no sessions".
    let log = lock_out(f.root.join(CODEX));
    let saved = lock_out(save(&f));
    assert_eq!(look(&mut usage), ["Codex · could not read", "Claude week · 82% left"]);
    assert!(frame(&mut usage, 140, 4)[2].contains("could not read sessions · Check session access"));
    drop((log, saved));
    assert_eq!(look(&mut usage), HEALTHY);

    let day = lock_out(f.root.join(CODEX).parent().unwrap().to_owned());
    let config = lock_out(f.root.join(CLAUDE_CONFIG));
    let saved = lock_out(save(&f));
    assert_eq!(look(&mut usage), ["Codex · could not read", "Claude · cannot read"]);
    drop((day, config, saved));
    assert_eq!(look(&mut usage), HEALTHY);

    let f = Fixture::folder();
    let Some(db) = omp_database(&f, "0.25", "0.27") else { return };
    let locked = lock_out(db.clone());
    let mut usage = omp_pane(&f, &db);
    assert_eq!(look(&mut usage), UNKNOWN);
    drop(locked);
    assert_eq!(look(&mut usage), ["Codex week · 75% left", "Claude week · 73% left"]);
}

#[test]
fn a_database_another_program_is_writing_keeps_its_last_reading_without_a_wait() {
    let f = Fixture::folder();
    let Some(db) = omp_database(&f, "0.25", "0.27") else { return };
    let mut usage = omp_pane(&f, &db);
    assert_eq!(look(&mut usage), ["Codex week · 75% left", "Claude week · 73% left"]);

    let writer = writing(&f, &db);
    let began = Instant::now();
    assert_eq!(look(&mut usage), ["Codex week · 75% left", "Claude week · 73% left"]);
    // A pane opened meanwhile has nothing to keep, and says so rather than
    // waiting. The pane gives a query one second; these two took a moment.
    assert_eq!(look(&mut omp_pane(&f, &db)), UNKNOWN);
    assert!(began.elapsed() < 5 * sqlite::QUERY_WAIT, "a locked database held the pane for {:?}", began.elapsed());
    drop(writer);

    exec(&db, "UPDATE cache SET value=replace(value,'0.25','0.5');");
    assert_eq!(look(&mut usage), ["Codex week · 50% left", "Claude week · 73% left"]);
}

#[test]
fn a_stale_source_keeps_its_figure_and_says_it_is_old() {
    let f = healthy();
    let mut usage = pane(&f);
    let old = ["Codex week · 60% last", "Claude week · 39% last"];

    // Both were read a minute before NOW and count as current for five minutes.
    usage.fixed_now = Some(NOW + 4 * MINUTE - 1);
    assert_eq!(look(&mut usage), HEALTHY);
    usage.fixed_now = Some(NOW + 4 * MINUTE);
    assert_eq!(look(&mut usage), old);
    assert_eq!(frame(&mut usage, 48, 2), [" Codex week · 60% · seen 5m ago", " Claude week · 39% · seen 5m ago"]);

    // A newer reading from another source replaces an old one whole.
    let now = NOW + 4 * MINUTE;
    write(&f, CLAUDE_CONFIG, &claude_cache(now, "fixture-current", r#"{"utilization":18}"#), now);
    assert_eq!(look(&mut usage), [old[0], "Claude week · 82% left"]);
    usage.fixed_now = Some(now + 5 * MINUTE);
    assert_eq!(look(&mut usage), [old[0], "Claude week · 82% last"]);

    // Once the window's reset time passes, the figure is no longer what is left.
    usage.fixed_now = Some(NOW + DAY);
    let rows = frame(&mut usage, 140, 4);
    assert!(rows[2].contains("Reset elapsed") && !rows[1].split('│').next().unwrap().contains("60%"), "{rows:?}");

    // OMP refreshes less often, so its reading is current for ten minutes.
    let f = Fixture::folder();
    let Some(db) = omp_database(&f, "0.25", "0.27") else { return };
    let mut usage = omp_pane(&f, &db);
    usage.fixed_now = Some(NOW + 10 * MINUTE - 1);
    assert_eq!(look(&mut usage), ["Codex week · 75% left", "Claude week · 73% left"]);
    usage.fixed_now = Some(NOW + 10 * MINUTE);
    assert_eq!(look(&mut usage), ["Codex week · 75% last", "Claude week · 73% last"]);
}

#[test]
fn a_source_in_an_unknown_shape_is_never_misread() {
    let f = healthy();
    let mut usage = pane(&f);
    let append = |data: &str| {
        let mut log = OpenOptions::new().append(true).open(f.root.join(CODEX)).unwrap();
        log.write_all(data.as_bytes()).unwrap();
        log.set_modified(mtime(NOW)).unwrap();
    };

    // A record Milo does not recognise as a limit report is not one. The last
    // report it understood stays.
    let stamp = format!("{}Z", iso(NOW));
    append(&codex_at(&stamp, 5, NOW + DAY, "null").replace("event_msg", "event").replace("token_count", "usage"));
    assert_eq!(look(&mut usage), HEALTHY);
    // A report it does recognise, with windows it cannot read, is the newest
    // word from Codex: there are no limits to show.
    append(&codex_snapshot(&stamp, r#"{"usedPercent":5,"windowMinutes":10080}"#, "null"));
    assert_eq!(look(&mut usage)[0], "Codex · no limits");

    // Renamed fields in Claude Code's status input leave the save as it was.
    let before = fs::read(save(&f)).unwrap();
    let renamed = br#"{"rate_limits":{"five_hour":{"utilization":0.05},"weekly":{"used_percentage":5}},
        "model":{"display_name":"Claude Test"}}"#;
    let mut line = Vec::new();
    assert_eq!(status::run(renamed, &mut line, Some(&save(&f)), NOW), 0);
    assert_eq!(line, b"Claude Test\n");
    assert_eq!(fs::read(save(&f)).unwrap(), before);
    assert_eq!(look(&mut usage)[1], HEALTHY[1]);

    // A cache with figures in another form is newer and has nothing usable.
    // One under another name is not a source at all.
    write(&f, CLAUDE_CONFIG, &claude_cache(NOW, "fixture-current", r#"{"utilization":"18%"}"#), NOW);
    assert_eq!(look(&mut usage)[1], "Claude · no limits");
    let moved = claude_cache(NOW, "fixture-current", r#"{"utilization":18}"#).replace("cachedUsage", "usage");
    write(&f, CLAUDE_CONFIG, &moved, NOW + 1);
    assert_eq!(look(&mut usage)[1], HEALTHY[1]);
    // A save without the fields Milo wrote is not a reading.
    f.write(&format!("{STATE}/{SAVED_LIMITS}"), r#"{"version":2,"limits":{"five_hour":{"used":23}}}"#);
    assert_eq!(look(&mut usage)[1], "Claude · unusable save");

    let f = Fixture::folder();
    let Some(db) = omp_database(&f, "0.25", "0.27") else { return };
    let mut usage = omp_pane(&f, &db);
    let read = ["Codex week · 75% left", "Claude week · 73% left"];
    assert_eq!(look(&mut usage), read);
    // A query that no longer fits the tables fails, and the last reading stays.
    exec(&db, "ALTER TABLE usage_history RENAME COLUMN used_fraction TO fraction;");
    assert_eq!(look(&mut usage), read);
    exec(&db, "ALTER TABLE usage_history RENAME COLUMN fraction TO used_fraction;");
    // A newer report whose limits changed form has no window Milo can show.
    let renamed =
        window("openai-codex:primary", "0.9", (NOW + DAY) * 1000, 10_080, "null").replace("usedFraction", "used");
    report(&db, "codex", "openai-codex", NOW * 1000, &format!("[{renamed}]"));
    report(&db, "claude", "anthropic", NOW * 1000, r#"{"anthropic:7d":0.9}"#);
    assert_eq!(look(&mut usage), ["Codex · no limits", "Claude · no limits"]);
}
