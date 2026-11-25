//! Keeps what each Rex terminal last reported as needing the person, and writes the menu's rows.
//!
//! The board holds the current state only, never a history: a record that goes
//! back to work, is cleared or loses its terminal leaves the board. A record
//! marked *seen* stays off the rows until its program reports again. Nothing
//! here is kept on disk.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt::Write as _;

use super::{Attention, Show, States};
use crate::json::Value;
use crate::rex;
use crate::usage::time::Timestamp;

const PREFIX: &[u8] = b"milo-attention ";
/// More rows than a menu can show. The menu says how many it left out.
const ROWS: usize = 32;
/// Rex allows a terminal 256 records. The script reports fewer, and no more are kept.
const RECORDS: usize = 32;
const LABEL_CHARS: usize = 48;

/// What a program's status asks of the person, most pressing first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Need {
    /// It is blocked until the person answers.
    Waiting,
    /// It failed and stopped.
    Error,
    /// It finished and the result has not been looked at.
    Done,
}

impl Need {
    fn parse(state: Value<'_>) -> Option<Self> {
        [("blocked", Self::Waiting), ("error", Self::Error), ("done", Self::Done)]
            .into_iter()
            .find_map(|(name, need)| state.is(name).then_some(need))
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Error => "error",
            Self::Done => "done",
        }
    }
}

/// What a waiting program asked for, when it said.
const ASKS: [&str; 3] = ["permission", "question", "auth"];

#[derive(Debug)]
struct Record {
    id: String,
    need: Need,
    asks: Option<&'static str>,
    app: Option<String>,
    /// When the record entered this state, in seconds.
    since: i64,
    /// When the program last reported it, as Rex timed it.
    at: Option<Timestamp>,
    seen: bool,
}

#[derive(Debug)]
struct Pane {
    block: String,
    name: String,
    tab: Option<String>,
    records: Vec<Record>,
}

/// Whether Rex can be heard, as the menu words it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Link {
    /// Rex has not been heard from yet, so there is nothing to say.
    #[default]
    Asking,
    /// No `rex` command was found to ask.
    Missing,
    /// No Rex server answers on this computer.
    Waiting,
    /// The server that answers runs on another computer, and is not asked about its panes.
    Remote,
    /// A script is attached. Rows are shown once its first full report is in.
    Connected,
}

impl Link {
    const fn name(self) -> &'static str {
        match self {
            Self::Asking => "",
            Self::Missing => "missing",
            Self::Waiting => "waiting",
            Self::Remote => "remote",
            Self::Connected => "connected",
        }
    }
}

#[derive(Default)]
pub(super) struct Board {
    link: Link,
    /// The host ID of the server now attached.
    host: Option<String>,
    /// Whether that server's first full report has arrived.
    ready: bool,
    panes: Vec<Pane>,
    /// The records marked seen, by block and record ID, with the state and report time that were seen.
    /// It outlives a lost connection so that the same finished record is not listed again after it.
    seen: HashMap<(String, String), (Need, Option<Timestamp>)>,
}

impl Board {
    /// Takes in one line the script printed. Anything else Rex prints is not a report.
    pub(super) fn line(&mut self, line: &[u8], now: i64) {
        let Some(report) = line.strip_prefix(PREFIX).and_then(Value::parse) else { return };
        let [server, ready, remote, block, clear] = report.fields(["server", "ready", "remote", "block", "clear"]);
        if remote.is_some() {
            self.forget();
            self.link = Link::Remote;
        } else if let Some(host) = server {
            self.forget();
            // A server that does not give a whole host ID is not one whose panes can be checked later.
            let host = host.string(36).filter(|host| rex::exact_id(OsStr::new(host.as_ref()), "").is_some());
            self.link = if host.is_some() { Link::Connected } else { Link::Waiting };
            self.host = host.map(|host| host.into_owned());
        } else if ready.is_some() {
            self.ready = self.host.is_some();
            // What was seen in a terminal that is gone, or reports something else now, is forgotten.
            let panes = &self.panes;
            self.seen.retain(|(block, id), _| {
                panes.iter().any(|pane| pane.block == *block && pane.records.iter().any(|record| record.id == *id))
            });
        } else if let Some(block) = block.and_then(|block| block.string(42))
            && rex::exact_id(OsStr::new(block.as_ref()), "block:").is_some()
            && self.host.is_some()
        {
            let records = if clear.is_some() { Vec::new() } else { self.records(&block, report, now) };
            self.place(&block, report, records);
        }
    }

    /// The connection ended. Nothing a lost server said is still shown as current.
    pub(super) fn lost(&mut self) {
        let remote = self.link == Link::Remote;
        self.forget();
        self.link = if remote { Link::Remote } else { Link::Waiting };
    }

    /// No `rex` command could be started.
    pub(super) fn missing(&mut self) {
        self.forget();
        self.link = Link::Missing;
    }

    /// Whether a server is attached and has reported in full, so that waits between tries can start over.
    pub(super) fn settled(&self) -> bool {
        self.ready
    }

    fn forget(&mut self) {
        self.host = None;
        self.ready = false;
        self.panes.clear();
    }

    /// The block's records as now reported, carrying over since when and whether each was seen.
    fn records(&mut self, block: &str, report: Value<'_>, now: i64) -> Vec<Record> {
        let [records] = report.fields(["records"]);
        let before = self.panes.iter().find(|pane| pane.block == block).map_or(&[][..], |pane| &pane.records);
        let mut current = Vec::new();
        for record in records.and_then(Value::elements).into_iter().flatten().take(RECORDS) {
            let [id, state, kind, app, at] = record.fields(["id", "state", "kind", "app", "at"]);
            let (Some(id), Some(need)) = (id.and_then(|id| id.string(128)), state.and_then(Need::parse)) else {
                continue;
            };
            let asks =
                kind.filter(|_| need == Need::Waiting).and_then(|kind| ASKS.into_iter().find(|ask| kind.is(ask)));
            let at = at.and_then(Timestamp::iso);
            let key = (block.to_owned(), id.into_owned());
            let same = before.iter().find(|old| old.id == key.1 && old.need == need && old.asks == asks);
            let (since, seen) = match same {
                // Rex repeats a status each time the program does, which is not a new state.
                Some(old) => (old.since, old.seen),
                None => {
                    let reported = at.map(|at| at.seconds).filter(|at| (1..=now).contains(at));
                    // After a reconnect, the record is the one seen only if it has not been reported since.
                    let seen =
                        self.seen.get(&key).is_some_and(|&(was, when)| was == need && at.is_some() && at <= when);
                    (reported.unwrap_or(now), seen)
                }
            };
            if seen {
                self.seen.insert(key.clone(), (need, at));
            } else {
                self.seen.remove(&key);
            }
            let app = app.and_then(|app| app.string(32)).filter(|app| short_name(app)).map(|app| app.into_owned());
            current.push(Record { id: key.1, need, asks, app, since, at, seen });
        }
        current
    }

    fn place(&mut self, block: &str, report: Value<'_>, records: Vec<Record>) {
        self.seen.retain(|(owner, id), _| owner != block || records.iter().any(|record| record.id == *id));
        self.panes.retain(|pane| pane.block != block);
        if records.is_empty() {
            return;
        }
        let [name, tab] = report.fields(["name", "tab"]).map(|label| label.and_then(|label| label.string(256)));
        let name = name.map(|name| plain(&name)).filter(|name| !name.is_empty()).unwrap_or_else(|| "Session".into());
        // Rex names an unnamed tab after its ID, which tells a person nothing.
        let tab = tab.map(|tab| plain(&tab)).filter(|tab| !tab.is_empty() && !generated_tab(tab));
        self.panes.push(Pane { block: block.to_owned(), name, tab, records });
    }

    /// Marks what the block's row shows as seen: its listed failures and finished work, never a wait.
    /// Records the chosen states leave out are not touched, and neither is any other block.
    pub(super) fn mark_seen(&mut self, block: &str, states: States) {
        let Some(pane) = self.panes.iter_mut().find(|pane| pane.block == block) else { return };
        for record in &mut pane.records {
            if record.need != Need::Waiting && states.include(record.need) && !record.seen {
                record.seen = true;
                self.seen.insert((pane.block.clone(), record.id.clone()), (record.need, record.at));
            }
        }
    }

    /// Writes the frame for the menu app, or nothing while a newly attached server is still reporting.
    pub(super) fn frame(&self, out: &mut String, choice: Attention) -> bool {
        if self.link == Link::Asking || self.link == Link::Connected && !self.ready {
            return false;
        }
        let dot = choice.show == Show::Dot;
        write!(out, r#"{{"version":1,"rex":"{}","dot":{dot},"entries":["#, self.link.name()).unwrap();
        let host = self.host.as_deref().unwrap_or_default();
        let mut rows: Vec<(&Pane, &Record)> =
            self.panes.iter().filter_map(|pane| Some((pane, row(pane, choice.states)?))).collect();
        rows.sort_by(|(a, x), (b, y)| (x.need, x.since, &a.block).cmp(&(y.need, y.since, &b.block)));
        let more = rows.len().saturating_sub(ROWS);
        for (index, (pane, record)) in rows.into_iter().take(ROWS).enumerate() {
            let separator = if index == 0 { "" } else { "," };
            let (block, name, state, since) = (&pane.block, rex::json(&pane.name), record.need.name(), record.since);
            write!(
                out,
                r#"{separator}{{"host":"{host}","block":"{block}","name":{name},"state":"{state}","since":{since}"#
            )
            .unwrap();
            if let Some(tab) = &pane.tab {
                write!(out, r#","tab":{}"#, rex::json(tab)).unwrap();
            }
            if let Some(app) = &record.app {
                write!(out, r#","app":"{app}""#).unwrap();
            }
            if let Some(asks) = record.asks {
                write!(out, r#","asks":"{asks}""#).unwrap();
            }
            out.push('}');
        }
        writeln!(out, r#"],"more":{more}}}"#).unwrap();
        true
    }
}

/// The record a pane's row shows: the most pressing that is chosen and not yet seen.
fn row(pane: &Pane, states: States) -> Option<&Record> {
    pane.records
        .iter()
        .filter(|record| states.include(record.need) && !record.seen)
        .min_by_key(|record| (record.need, record.since))
}

/// The characters Rex's status protocol allows in a program's short name.
fn short_name(app: &str) -> bool {
    !app.is_empty() && app.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'+' | b'-'))
}

fn generated_tab(label: &str) -> bool {
    label.strip_prefix("Window ").is_some_and(|id| id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A session or tab label as one short line. A label is whatever someone typed, so characters
/// that draw nothing or turn the text around are left out before it reaches the menu.
fn plain(label: &str) -> String {
    let hidden = |c: char| {
        c.is_control()
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
    };
    label.trim().chars().filter(|&c| !hidden(c)).take(LABEL_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "27ea019d-95c6-4fba-ab3c-d19b89cbb459";
    const A: &str = "block:01a21295-d49e-7bcb-a89c-ef73e104fd7e";
    const B: &str = "block:01a2129a-2773-77db-94ac-4c4b3ecf330a";
    /// Midnight on 18 November 2026, UTC.
    const NOW: i64 = 1_794_960_000;
    const ALL: States = States { waiting: true, errors: true, done: true };

    fn at(seconds: i64) -> String {
        format!("2026-11-18T00:{:02}:{:02}.5+00:00", seconds / 60, seconds % 60)
    }

    fn record(id: &str, state: &str, seconds: i64) -> String {
        format!(r#"{{"id":"{id}","state":"{state}","app":"codex","at":"{}"}}"#, at(seconds))
    }

    fn block(board: &mut Board, block: &str, name: &str, records: &[String], now: i64) {
        let line = format!(
            r#"milo-attention {{"block":"{block}","session":"session:x","name":"{name}","tab":"Window 4949ea30","records":[{}]}}"#,
            records.join(",")
        );
        board.line(line.as_bytes(), now);
    }

    fn connected() -> Board {
        let mut board = Board::default();
        board.line(format!(r#"milo-attention {{"server":"{HOST}"}}"#).as_bytes(), NOW);
        board.line(br#"milo-attention {"ready":true}"#, NOW);
        board
    }

    fn frame(board: &Board, states: States) -> String {
        let mut out = String::new();
        assert!(board.frame(&mut out, Attention { show: Show::Dot, states }));
        out
    }

    /// Each row as `name state since`, in the order the menu lists them.
    fn rows(board: &Board, states: States) -> Vec<String> {
        let out = frame(board, states);
        let [entries] = Value::parse(out.as_bytes()).unwrap().fields(["entries"]);
        entries
            .unwrap()
            .elements()
            .unwrap()
            .map(|entry| {
                let [name, state, since] = entry.fields(["name", "state", "since"]);
                let age = since.unwrap().integer().unwrap() - NOW;
                format!("{} {} {age}", name.unwrap().string(64).unwrap(), state.unwrap().string(8).unwrap())
            })
            .collect()
    }

    #[test]
    fn a_state_is_listed_while_it_holds_and_leaves_when_work_resumes() {
        let mut board = connected();
        block(&mut board, A, "one", &[record("", "blocked", 0)], NOW);
        assert_eq!(rows(&board, ALL), ["one waiting 0"]);
        // The program says the same again: still one row, waiting since the first time.
        block(&mut board, A, "one", &[record("", "blocked", 30)], NOW + 30);
        assert_eq!(rows(&board, ALL), ["one waiting 0"]);
        // Work resumes. The script reports the block as holding nothing that needs anyone.
        board.line(format!(r#"milo-attention {{"block":"{A}","clear":true}}"#).as_bytes(), NOW + 40);
        assert!(rows(&board, ALL).is_empty());
        block(&mut board, A, "one", &[record("", "error", 50)], NOW + 50);
        block(&mut board, A, "one", &[record("", "done", 60)], NOW + 60);
        assert_eq!(rows(&board, ALL), ["one done 60"]);
    }

    #[test]
    fn each_state_is_chosen_on_its_own_and_the_most_pressing_leads() {
        let mut board = connected();
        block(&mut board, A, "one", &[record("", "done", 0), record("sub", "error", 5)], NOW + 5);
        block(&mut board, B, "two", &[record("", "blocked", 9)], NOW + 9);
        assert_eq!(rows(&board, ALL), ["two waiting 9", "one error 5"]);
        assert_eq!(rows(&board, States::default()), ["two waiting 9", "one error 5"]);
        let only = |waiting, errors, done| States { waiting, errors, done };
        assert_eq!(rows(&board, only(false, false, true)), ["one done 0"]);
        assert_eq!(rows(&board, only(true, false, false)), ["two waiting 9"]);
        assert_eq!(rows(&board, only(false, true, false)), ["one error 5"]);
        let out = frame(&board, only(true, false, false));
        assert!(out.contains(r#""rex":"connected","dot":true"#) && out.ends_with("\"more\":0}\n"), "{out}");
        assert!(
            out.contains(&format!(r#""host":"{HOST}","block":"{B}""#)) && out.contains(r#""app":"codex""#),
            "{out}"
        );
        // A tab Rex named after its ID is not shown as a name.
        assert!(!out.contains("tab"), "{out}");
    }

    #[test]
    fn marking_one_row_seen_leaves_every_other_row_and_lasts_until_a_new_report() {
        let mut board = connected();
        block(&mut board, A, "one", &[record("", "done", 0)], NOW);
        block(&mut board, B, "two", &[record("", "done", 1), record("ask", "blocked", 2)], NOW + 2);
        // A wait cannot be marked seen, and the finished record under it is not on the row.
        board.mark_seen(B, States { done: false, ..ALL });
        assert_eq!(rows(&board, ALL), ["two waiting 2", "one done 0"]);
        board.mark_seen(A, ALL);
        assert_eq!(rows(&board, ALL), ["two waiting 2"]);
        // The wait ends; the other pane's finished work is still there to be seen.
        block(&mut board, B, "two", &[record("", "done", 1)], NOW + 10);
        assert_eq!(rows(&board, ALL), ["two done 1"]);
        // A repeat of what was seen stays seen. The same program finishing again is new.
        block(&mut board, A, "one", &[record("", "done", 20)], NOW + 20);
        assert_eq!(rows(&board, ALL), ["two done 1"]);
        board.line(format!(r#"milo-attention {{"block":"{A}","clear":true}}"#).as_bytes(), NOW + 30);
        block(&mut board, A, "one", &[record("", "done", 40)], NOW + 40);
        assert_eq!(rows(&board, ALL), ["two done 1", "one done 40"]);
    }

    #[test]
    fn a_lost_server_counts_nothing_and_a_reconnect_starts_from_what_it_reports() {
        let mut board = connected();
        block(&mut board, A, "one", &[record("", "done", 0)], NOW);
        block(&mut board, B, "two", &[record("", "blocked", 1)], NOW + 1);
        board.mark_seen(A, ALL);
        board.lost();
        let out = frame(&board, ALL);
        assert_eq!(out, "{\"version\":1,\"rex\":\"waiting\",\"dot\":true,\"entries\":[],\"more\":0}\n");

        // While the new server is still reporting, the menu keeps the frame it has.
        board.line(format!(r#"milo-attention {{"server":"{HOST}"}}"#).as_bytes(), NOW + 60);
        block(&mut board, A, "one", &[record("", "done", 0)], NOW + 60);
        assert!(!board.frame(&mut String::new(), Attention::default()));
        // The pane that was waiting has gone: it is not carried over. What was seen stays seen.
        board.line(br#"milo-attention {"ready":true}"#, NOW + 60);
        assert!(rows(&board, ALL).is_empty());

        // After another gap the seen record was reported again, so it is listed, from when Rex timed it.
        board.lost();
        board.line(format!(r#"milo-attention {{"server":"{HOST}"}}"#).as_bytes(), NOW + 120);
        block(&mut board, A, "one", &[record("", "done", 90)], NOW + 120);
        board.line(br#"milo-attention {"ready":true}"#, NOW + 120);
        assert_eq!(rows(&board, ALL), ["one done 90"]);
    }

    #[test]
    fn a_remote_or_missing_rex_is_named_and_reports_no_pane() {
        let mut board = connected();
        block(&mut board, A, "one", &[record("", "blocked", 0)], NOW);
        board.line(br#"milo-attention {"remote":true}"#, NOW);
        board.lost();
        assert!(frame(&board, ALL).contains(r#""rex":"remote","dot":true,"entries":[]"#));
        board.missing();
        assert!(frame(&board, ALL).contains(r#""rex":"missing""#));
    }

    #[test]
    fn untrusted_reports_are_bounded_and_labels_are_made_plain() {
        let mut board = connected();
        for line in [
            "listening: 6 handlers, every session; Ctrl-C to stop".to_owned(),
            r#"milo-attention {"block":"block:short","records":[{"id":"","state":"blocked"}]}"#.to_owned(),
            format!(r#"milo-attention {{"block":"{A}","records":[{{"id":"","state":"working"}},{{"state":"done"}}]}}"#),
            r#"milo-attention {"server":"not a host"}"#.to_owned(),
        ] {
            board.line(line.as_bytes(), NOW);
        }
        // The last line named no server, so nothing is attached and nothing is listed.
        assert!(frame(&board, ALL).contains(r#""entries":[]"#));

        let mut board = connected();
        let line = format!(
            r#"milo-attention {{"block":"{A}","name":" a\u202e\"b\u0007 ","tab":"Tests","records":[
                {{"id":"","state":"blocked","kind":"auth","app":"co dex","at":"2999-01-01T00:00:00Z"}}]}}"#
        )
        .replace('\n', "");
        board.line(line.as_bytes(), NOW);
        let out = frame(&board, ALL);
        // An unsafe program name is left out, and a time in the future is not believed.
        assert!(
            out.contains(&format!(r#""name":"a\"b","state":"waiting","since":{NOW},"tab":"Tests","asks":"auth"}}"#)),
            "{out}"
        );
        let many: Vec<String> = (0..40).map(|n| record(&format!("r{n}"), "done", 0)).collect();
        block(&mut board, B, "two", &many, NOW);
        assert_eq!(board.panes.iter().find(|pane| pane.block == B).unwrap().records.len(), RECORDS);
    }
}
