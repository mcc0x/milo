//! Reads today's activity from OpenCode's local session database without running OpenCode.
//!
//! OpenCode saves no limits on disk, so this source never yields one. The
//! queries name only the session tables and return sums and times: message
//! text is parsed inside SQLite and credentials are never selected.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::json::Value;
use crate::when::{self, Detail};

use super::Today;
use super::sqlite::{self, Poll};

const GUARD: &str = "PRAGMA query_only=ON;\nPRAGMA trusted_schema=OFF;\n";

/// The columns each store must have before its totals are queried.
const COLUMNS: [(&str, &str); 14] = [
    ("session_v2", "id"),
    ("session_v2", "version"),
    ("session_v2", "time_created"),
    ("session_v2", "fork_session_id"),
    ("session_message", "session_id"),
    ("session_message", "type"),
    ("session_message", "time_created"),
    ("session_message", "data"),
    ("session", "id"),
    ("session", "version"),
    ("session", "time_created"),
    ("message", "session_id"),
    ("message", "time_created"),
    ("message", "data"),
];
const SESSION_V2: u16 = 0x000f;
const V2_COLUMNS: u16 = 0x00ff;
const V1_COLUMNS: u16 = 0x3f00;

const SHAPE: &str = "
SELECT t.name AS tbl, c.name AS col
FROM sqlite_schema t, pragma_table_info(t.name) c
WHERE t.type='table' AND t.name IN ('session','message','session_v2','session_message')
  AND c.name IN ('id','session_id','type','version','fork_session_id','time_created','data');
";

/// A step's own JSON, or NULL when it is not JSON at all.
const DATA: &str = "CASE WHEN json_valid(m.data) THEN m.data END AS data";

/// One row for the steps a store selected. A step counts as invalid when it
/// is not JSON, when its token counts are not whole numbers at or above zero
/// inside an object, or when its cost is not a number at or above zero, so
/// that one bad row cannot hide inside a sum.
const TOTALS: &str = "
SELECT count(*) AS steps, max(at) AS last_ms, coalesce(sum(unverified),0) AS unverified,
       coalesce(sum(json_type(data) IS NOT 'object'
           OR coalesce(json_type(data,'$.tokens'),'null') NOT IN ('object','null')
           OR coalesce(json_type(data,'$.tokens.cache'),'null') NOT IN ('object','null')
           OR EXISTS (SELECT 1 FROM json_tree(data,'$.tokens')
                      WHERE (type NOT IN ('integer','null') AND
                             (type <> 'object' OR fullkey NOT IN ('$.tokens','$.tokens.cache')))
                             OR atom<0)
           OR coalesce(json_type(data,'$.cost'),'null') NOT IN ('integer','real','null')
           OR coalesce(json_extract(data,'$.cost')<0,0)),0) AS invalid,
       sum(coalesce(json_extract(data,'$.tokens.input'),0)) AS input,
       sum(coalesce(json_extract(data,'$.tokens.output'),0)) AS output,
       sum(coalesce(json_extract(data,'$.tokens.reasoning'),0)) AS reasoning,
       sum(coalesce(json_extract(data,'$.tokens.cache.read'),0)) AS cache_read,
       sum(coalesce(json_extract(data,'$.tokens.cache.write'),0)) AS cache_write,
       sum(json_extract(data,'$.cost')) AS cost, count(json_extract(data,'$.cost')) AS priced
FROM step;
";

/// Which generation of session tables a database uses. Only one generation is
/// ever counted, because an upgraded database holds its steps in both.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Store {
    /// `session` and `message`, written by OpenCode 1.2 to 1.19.
    V1,
    /// `session_v2` and `session_message`, written by OpenCode 2. `legacy`
    /// says the 1.x tables it migrated from are still beside them.
    V2 { legacy: bool },
}

impl Store {
    /// `session_v2` marks an OpenCode 2 database. Late 1.x releases already
    /// create an unused `session_message`, so that table alone does not.
    /// A generation that is present must have every column Milo reads.
    fn detect(shape: &[u8]) -> Option<Self> {
        let mut seen = 0_u16;
        // sqlite3 prints nothing for a select that returns no rows.
        if !shape.is_empty() {
            for row in Value::parse(shape)?.elements()? {
                let [table, column] = row.fields(["tbl", "col"]);
                let (table, column) = (table?.string(32)?, column?.string(32)?);
                if let Some(i) = COLUMNS.iter().position(|&(t, c)| t == table && c == column) {
                    seen |= 1 << i;
                }
            }
        }
        let old = seen & V1_COLUMNS;
        if seen & SESSION_V2 == 0 {
            return (old == V1_COLUMNS).then_some(Self::V1);
        }
        (seen & V2_COLUMNS == V2_COLUMNS && (old == 0 || old == V1_COLUMNS)).then_some(Self::V2 { legacy: old != 0 })
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::V1 => "OpenCode 1 session tables",
            Self::V2 { .. } => "OpenCode 2 session tables",
        }
    }

    /// Totals for assistant steps created from `since` up to and including `now`.
    ///
    /// A step is unverified when its session was created by a release line
    /// Milo was not checked against. So is any 1.x message from today whose
    /// session has nothing from today in the 2.x tables: an upgrade still
    /// copying sessions, or a 1.x binary still writing after one.
    pub(super) fn totals(self, since: i64, now: i64) -> String {
        let (since, until) = (since.saturating_mul(1000), now.saturating_add(1).saturating_mul(1000));
        // A session's own messages are never older than the session, so
        // older ones are history that a fork copied in.
        let old = format!(
            "FROM session s CROSS JOIN message m ON m.session_id=s.id
             WHERE m.time_created>=max({since},s.time_created) AND m.time_created<{until}"
        );
        // The upgrade does not carry a 1.x fork's origin over, so a session
        // 1.x created is held to the same rule as in the old tables.
        let new = format!(
            "SELECT m.time_created AS at, coalesce(CAST(s.version AS INTEGER),0) NOT IN (1,2) AS unverified, {DATA}
             FROM session_message m JOIN session_v2 s ON s.id=m.session_id
             WHERE m.type='assistant' AND m.time_created>={since} AND m.time_created<{until}
               AND ((s.fork_session_id IS NULL AND CAST(s.version AS INTEGER) IS NOT 1)
                    OR m.time_created>=s.time_created)"
        );
        let steps = match self {
            Self::V1 => format!(
                "SELECT m.time_created AS at, CAST(s.version AS INTEGER) IS NOT 1 AS unverified, {DATA} {old}
                 AND CASE WHEN json_valid(m.data)
                          THEN json_type(m.data) IS NOT 'object' OR json_extract(m.data,'$.role')='assistant'
                          ELSE 1 END"
            ),
            Self::V2 { legacy: false } => new,
            Self::V2 { legacy: true } => format!(
                "{new} UNION ALL SELECT NULL, 1, NULL {old} AND NOT EXISTS (
                     SELECT 1 FROM session_message n WHERE n.session_id=m.session_id AND n.time_created>={since})"
            ),
        };
        format!("{GUARD}WITH step AS ({steps}){TOTALS}")
    }
}

/// Token kinds as OpenCode records them. They do not overlap.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(super) struct Tokens {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    /// New input, output and cache writes, as for Claude Code. Cache reads stay apart.
    pub(super) fn fresh(self) -> u64 {
        [self.output, self.reasoning, self.cache_write].into_iter().fold(self.input, u64::saturating_add)
    }

    /// Every kind must be a usable sum, or none of them is trusted.
    fn sums(row: Value<'_>) -> Option<Self> {
        let [input, output, reasoning, cache_read, cache_write] =
            row.fields(["input", "output", "reasoning", "cache_read", "cache_write"]).map(count);
        Some(Self {
            input: input?,
            output: output?,
            reasoning: reasoning?,
            cache_read: cache_read?,
            cache_write: cache_write?,
        })
    }
}

/// OpenCode's own estimate in US dollars, not a bill: a subscription or an
/// unpriced model records zero.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Cost {
    pub usd: f64,
    /// Whether every counted step recorded a cost.
    pub complete: bool,
}

impl Cost {
    /// A zero estimate says nothing was priced, not that the work was free.
    /// A `+` marks a sum that some steps did not contribute to.
    pub(super) fn text(self) -> Option<String> {
        let more = if self.complete { "" } else { "+" };
        match self.usd {
            usd if usd >= 0.005 => Some(format!("${usd:.2}{more}")),
            usd if usd > 0.0 => Some(format!("<$0.01{more}")),
            _ => None,
        }
    }
}

/// Assistant steps recorded since local midnight, for every project in the database.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Activity {
    /// The local midnight the totals start from.
    pub since: i64,
    pub store: Store,
    pub steps: u64,
    pub tokens: Tokens,
    pub cost: Option<Cost>,
    /// When the newest counted step was created.
    pub last: Option<i64>,
}

impl Activity {
    /// Refuse totals that include rows Milo cannot vouch for.
    fn parse(bytes: &[u8], store: Store, since: i64) -> Option<Self> {
        let mut rows = Value::parse(bytes)?.elements()?;
        let row = rows.next().filter(|_| rows.next().is_none())?;
        let [steps, last, invalid, unverified, cost, priced] =
            row.fields(["steps", "last_ms", "invalid", "unverified", "cost", "priced"]);
        if invalid?.unsigned()? != 0 || unverified?.unsigned()? != 0 {
            return None;
        }
        let steps = steps?.unsigned()?;
        let tokens = Tokens::sums(row)?;
        let cost = match cost.filter(|value| !value.null()) {
            Some(value) => {
                let usd = value.number().filter(|usd| *usd >= 0.0)?;
                Some(Cost { usd, complete: priced?.unsigned()? == steps })
            }
            None => None,
        };
        let last = match last.filter(|value| !value.null()) {
            Some(value) => Some(value.integer()?.div_euclid(1000)),
            None => None,
        };
        Some(Self { since, store, steps, tokens, cost, last })
    }
}

/// A sum of whole token counts, or nothing when no step matched.
fn count(value: Option<Value<'_>>) -> Option<u64> {
    let value = value?;
    if value.null() { Some(0) } else { value.unsigned() }
}

/// What the local database could tell Milo. Only `Today` carries figures.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(super) enum Report {
    /// No database: OpenCode is not installed, has saved nothing here, or
    /// left only an empty or foreign file under the database's name.
    #[default]
    Missing,
    /// A database that could not be queried in time.
    Unreadable,
    /// Tables, rows or OpenCode versions Milo has not been checked against.
    Unsupported,
    Today(Activity),
}

impl Report {
    /// Says in words what was read and from where, keeping each kind of figure apart.
    pub(super) fn describe(&self, path: Option<&Path>, now: i64) -> Vec<String> {
        let (Some(path), false) = (path, *self == Self::Missing) else {
            return vec!["OpenCode activity: no local database".into()];
        };
        let activity = match self {
            Self::Missing | Self::Unreadable => format!("unknown; could not read {path:?}"),
            Self::Unsupported => {
                format!("unknown; {path:?} holds tables, rows or OpenCode versions Milo has not checked")
            }
            Self::Today(today) if today.steps == 0 => {
                format!("no steps recorded today ({} in {path:?})", today.store.label())
            }
            Self::Today(today) => {
                let Tokens { cache_read, .. } = today.tokens;
                let newest = today.last.map_or(String::new(), |at| {
                    format!(", newest {} ago", when::duration(now.saturating_sub(at), Detail::Rounded))
                });
                format!(
                    "{} tokens and {cache_read} cache reads in {} {} today{newest} ({} in {path:?})",
                    today.tokens.fresh(),
                    today.steps,
                    if today.steps == 1 { "step" } else { "steps" },
                    today.store.label()
                )
            }
        };
        let cost = match self {
            Self::Today(Activity { cost: Some(cost), .. }) => cost.text(),
            _ => None,
        };
        vec![
            format!("OpenCode activity: {activity}"),
            format!(
                "OpenCode cost: {}",
                cost.map_or("unknown".into(), |usd| format!("{usd} today, as OpenCode estimated it"))
            ),
            "OpenCode limits: unknown; OpenCode saves none locally".into(),
        ]
    }
}

/// Honour OpenCode's own overrides without running it.
pub(super) fn database(home: Option<&Path>) -> Option<PathBuf> {
    let variable = |name| std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from);
    database_in(home, variable("XDG_DATA_HOME").as_deref(), variable("OPENCODE_DB").as_deref())
}

/// Only the database OpenCode's releases share is read. Builds on other
/// channels keep `opencode-<channel>.db` beside it.
pub(super) fn database_in(home: Option<&Path>, data: Option<&Path>, named: Option<&Path>) -> Option<PathBuf> {
    match named {
        Some(named) if named.is_absolute() => return Some(named.to_owned()),
        Some(named) if named == Path::new(":memory:") => return None,
        _ => {}
    }
    let data = match data {
        Some(data) if data.is_absolute() => data.to_owned(),
        // OpenCode resolves a relative directory against its own working directory.
        Some(_) => return None,
        None => home?.join(".local/share"),
    };
    Some(data.join("opencode").join(named.unwrap_or(Path::new("opencode.db"))))
}

pub(super) fn poll(path: Option<PathBuf>, wait: Duration) -> Poll<Report> {
    Poll::new(path, wait, |path, now, wait| Some(read(path, now, wait)))
}

/// Both queries share one wait, so a busy database costs at most that long.
pub(super) fn read(path: &Path, now: i64, wait: Duration) -> Report {
    if !sqlite::is_database(path) {
        return Report::Missing;
    }
    let deadline = Instant::now() + wait;
    let shape = [GUARD, SHAPE].concat();
    let Some(shape) = sqlite::query(sqlite::executable(), path, &shape, wait) else { return Report::Unreadable };
    let Some(store) = Store::detect(&shape) else { return Report::Unsupported };
    let since = Today::at(now, when::local_day(now)).since;
    let left = deadline.saturating_duration_since(Instant::now());
    match sqlite::query(sqlite::executable(), path, &store.totals(since, now), left) {
        Some(totals) => Activity::parse(&totals, store, since).map_or(Report::Unsupported, Report::Today),
        None => Report::Unreadable,
    }
}
