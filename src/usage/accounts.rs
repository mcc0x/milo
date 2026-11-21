//! Figures only a provider's own service reports, for accounts the user turned on.
//!
//! Nothing here is a share of a limit. A balance, a spend and a count of
//! credits each keep their own unit, and none of them is turned into a
//! percentage or compared with another.

use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::os::fd::RawFd;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::json::Value;
use crate::preferences::{Show, UsageAccount, UsageAccounts};
use crate::when::{self, DAY, Detail, HOUR, MINUTE};
use crate::{fs_ops, paths};

use super::{CLOCK_SKEW, Presence, http, sqlite};

/// A deliberate reload asks again no sooner than this after the last request.
const MANUAL_REST: i64 = 30;
/// A refused key is not offered again for a while: the provider already said no.
const REFUSED_REST: i64 = 30 * MINUTE;
/// How long to stay away after a rate limit that named no wait of its own.
const LIMITED_REST: i64 = 5 * MINUTE;
/// A reading older than this no longer says anything about the account.
const KEPT: i64 = DAY;
const HTTP_WAIT: Duration = Duration::from_secs(8);
const SETTINGS_LIMIT: u64 = 8 * 1024;
const CACHE_LIMIT: u64 = 4 * 1024;
const SETTINGS_FILE: &str = "usage-accounts.conf";
/// Names a loopback port that stands in for every provider. Tests set it.
const TEST_PORT: &str = "MILO_TEST_USAGE_PORT";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Currency {
    Usd,
    Cny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unit {
    Money(Currency),
    /// Devin's agent compute units.
    Acus,
    /// GitHub's AI credits, the unit Copilot usage is billed in.
    AiCredits,
}

/// What an amount measures. A kind is never converted into another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// Prepaid money still on the account.
    Balance,
    /// The part of a balance the provider gave rather than the user paid in.
    Granted,
    /// What is left of a spending limit set on the key.
    LimitLeft,
    /// Everything the key has consumed.
    Used,
    /// Consumed in the provider's current day.
    UsedToday,
    /// Consumed in the provider's current month.
    UsedThisMonth,
}

/// An amount in the provider's own unit, with what it measures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Figure {
    pub(super) kind: Kind,
    pub(super) unit: Unit,
    pub(super) value: f64,
}

impl Figure {
    const KINDS: [(Kind, &'static str, &'static str); 6] = [
        (Kind::Balance, "balance", " balance"),
        (Kind::Granted, "granted", " granted"),
        (Kind::LimitLeft, "limit_left", " key limit left"),
        (Kind::Used, "used", " used"),
        (Kind::UsedToday, "used_today", " today"),
        (Kind::UsedThisMonth, "used_this_month", " this month"),
    ];
    const UNITS: [(Unit, &'static str); 4] = [
        (Unit::Money(Currency::Usd), "usd"),
        (Unit::Money(Currency::Cny), "cny"),
        (Unit::Acus, "acus"),
        (Unit::AiCredits, "ai_credits"),
    ];

    /// The amount alone, such as `¥110.00` or `12.5 ACUs`.
    pub(super) fn amount(self) -> String {
        // Two decimals at most, without trailing zeros: 12.50 reads as 12.5.
        let count = |value: f64| {
            let text = format!("{value:.2}");
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        };
        match self.unit {
            Unit::Money(Currency::Usd) => format!("${:.2}", self.value),
            Unit::Money(Currency::Cny) => format!("¥{:.2}", self.value),
            Unit::Acus if self.value == 1.0 => "1 ACU".into(),
            Unit::Acus => format!("{} ACUs", count(self.value)),
            Unit::AiCredits if self.value == 1.0 => "1 AI credit".into(),
            Unit::AiCredits => format!("{} AI credits", count(self.value)),
        }
    }

    /// The amount with what it measures, such as `¥110.00 balance`.
    pub(super) fn text(self) -> String {
        let words = Self::KINDS.iter().find(|(kind, ..)| *kind == self.kind).map_or("", |(_, _, words)| words);
        format!("{}{words}", self.amount())
    }

    fn save(self, out: &mut String) {
        let kind = Self::KINDS.iter().find(|(kind, ..)| *kind == self.kind).map_or("", |(_, name, _)| name);
        let unit = Self::UNITS.iter().find(|(unit, _)| *unit == self.unit).map_or("", |(_, name)| name);
        write!(out, r#"{{"kind":"{kind}","unit":"{unit}","value":{}}}"#, self.value).expect("writing to a string");
    }

    fn saved(value: Value<'_>) -> Option<Self> {
        let [kind, unit, amount] = value.fields(["kind", "unit", "value"]);
        let (kind, unit) = (kind?, unit?);
        Some(Self {
            kind: Self::KINDS.iter().find(|(_, name, _)| kind.is(name))?.0,
            unit: Self::UNITS.iter().find(|(_, name)| unit.is(name))?.0,
            value: amount?.number().filter(|value| *value >= 0.0)?,
        })
    }
}

/// What a provider last said about an account.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Reading {
    /// When Milo received the answer.
    pub(super) observed: i64,
    pub(super) figure: Figure,
    pub(super) details: [Option<Figure>; 2],
    /// When the period the figure counts ends. A balance has no period.
    pub(super) until: Option<i64>,
    /// The provider says the balance cannot pay for a request.
    pub(super) depleted: bool,
}

impl Reading {
    /// A reading from a past period, a past day or a clock that was ahead says nothing now.
    fn usable(&self, now: i64) -> bool {
        self.observed <= now + CLOCK_SKEW
            && now.saturating_sub(self.observed) <= KEPT
            && self.until.is_none_or(|until| now < until)
    }

    fn save(&self, out: &mut String) {
        write!(out, r#"{{"observed":{},"figure":"#, self.observed).expect("writing to a string");
        self.figure.save(out);
        out.push_str(r#","details":["#);
        for (i, detail) in self.details.iter().flatten().enumerate() {
            if i > 0 {
                out.push(',');
            }
            detail.save(out);
        }
        out.push(']');
        if let Some(until) = self.until {
            write!(out, r#","until":{until}"#).expect("writing to a string");
        }
        write!(out, r#","depleted":{}}}"#, self.depleted).expect("writing to a string");
    }

    fn saved(value: Value<'_>) -> Option<Self> {
        let [observed, figure, details, until, depleted] =
            value.fields(["observed", "figure", "details", "until", "depleted"]);
        let mut more = details?.elements()?.map(Figure::saved);
        Some(Self {
            observed: observed?.integer()?,
            figure: Figure::saved(figure?)?,
            details: [more.next().flatten(), more.next().flatten()],
            until: until.and_then(Value::integer),
            depleted: depleted?.boolean()?,
        })
    }
}

/// Why an account has no new reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    /// No key is named in the environment or the settings file.
    NoKey,
    /// The key is there, but not the organization or user it should be asked about.
    NoScope,
    /// The settings file could be read by someone else, or is not a plain file.
    OpenFile,
    /// A setting holds something Milo will not put in a request.
    BadSetting,
    /// The provider did not accept the key.
    Refused,
    /// The key is valid but may not read this, or the plan does not offer it.
    Denied,
    Limited,
    /// No answer: no network, no curl, a timeout or a server error.
    Unreachable,
    /// An answer in a form Milo does not know.
    Unexpected,
    /// The provider answered that it has recorded nothing for this period.
    Nothing,
}

impl Failure {
    const NAMES: [(Self, &'static str, &'static str); 10] = [
        (Self::NoKey, "no_key", "no key is set"),
        (Self::NoScope, "no_scope", "the account to ask about is not set"),
        (Self::OpenFile, "open_file", "the settings file is not private"),
        (Self::BadSetting, "bad_setting", "a setting cannot be sent"),
        (Self::Refused, "refused", "the provider refused the key"),
        (Self::Denied, "denied", "the provider denied access to this figure"),
        (Self::Limited, "limited", "the provider rate limited the request"),
        (Self::Unreachable, "unreachable", "the provider could not be reached"),
        (Self::Unexpected, "unexpected", "the reply was not understood"),
        (Self::Nothing, "nothing", "the provider reported no usage for this period"),
    ];

    fn name(self) -> &'static str {
        Self::NAMES.iter().find(|(failure, ..)| *failure == self).map_or("", |(_, name, _)| name)
    }

    fn words(self) -> &'static str {
        Self::NAMES.iter().find(|(failure, ..)| *failure == self).map_or("", |(_, _, words)| words)
    }

    /// A failure of what is set up here says nothing about the account, so
    /// an earlier reading is not shown beside it.
    fn local(self) -> bool {
        matches!(self, Self::NoKey | Self::NoScope | Self::OpenFile | Self::BadSetting)
    }
}

/// The result of one look: at the saved answer, and at the provider when one was due.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Fetched {
    pub(super) reading: Option<Reading>,
    pub(super) failure: Option<Failure>,
    /// When the provider was last asked.
    asked: i64,
    /// The earliest time to ask it again unprompted.
    next: i64,
}

impl UsageAccount {
    pub(super) const fn id(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
            Self::Devin => "devin",
            Self::Copilot => "copilot",
        }
    }

    /// A name short enough for the narrowest rail.
    pub(super) const fn short(self) -> &'static str {
        match self {
            Self::DeepSeek => "DS",
            Self::OpenRouter => "OR",
            Self::Devin => "Dv",
            Self::Copilot => "Cp",
        }
    }

    const fn host(self) -> &'static str {
        match self {
            Self::DeepSeek => "api.deepseek.com",
            Self::OpenRouter => "openrouter.ai",
            Self::Devin => "api.devin.ai",
            Self::Copilot => "api.github.com",
        }
    }

    /// What the provider is asked for, in doctor's words.
    const fn subject(self) -> &'static str {
        match self {
            Self::DeepSeek => "balance",
            Self::OpenRouter => "key spend",
            Self::Devin => "ACUs",
            Self::Copilot => "AI credits",
        }
    }

    pub(super) const fn key_name(self) -> &'static str {
        match self {
            Self::DeepSeek => "MILO_DEEPSEEK_API_KEY",
            Self::OpenRouter => "MILO_OPENROUTER_API_KEY",
            Self::Devin => "MILO_DEVIN_API_KEY",
            Self::Copilot => "MILO_GITHUB_TOKEN",
        }
    }

    /// The setting that names whose figures to ask for, where the key alone does not.
    pub(super) const fn scope_name(self) -> Option<&'static str> {
        match self {
            Self::DeepSeek | Self::OpenRouter => None,
            Self::Devin => Some("MILO_DEVIN_ORG"),
            Self::Copilot => Some("MILO_GITHUB_USER"),
        }
    }

    /// How often the provider is asked. Balances move with every request;
    /// the consumption reports are aggregated by their providers.
    const fn interval(self) -> i64 {
        match self {
            Self::DeepSeek | Self::OpenRouter => 5 * MINUTE,
            Self::Devin | Self::Copilot => 15 * MINUTE,
        }
    }

    /// A reading is as good as current until two requests could have replaced it.
    const fn fresh_for(self) -> i64 {
        2 * self.interval() + MINUTE
    }

    fn valid_scope(self, scope: &str) -> bool {
        let named = |text: &str, max: usize, extra: &[u8]| {
            (1..=max).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
        };
        match self {
            Self::DeepSeek | Self::OpenRouter => false,
            Self::Devin => scope.strip_prefix("org-").is_some_and(|id| named(id, 64, b"-_")),
            Self::Copilot => named(scope, 39, b"-"),
        }
    }

    /// The path and query to ask for, and when the period it covers ends.
    fn question(self, scope: Option<&str>, now: i64) -> (String, Option<i64>) {
        let scope = scope.unwrap_or_default();
        match self {
            Self::DeepSeek => ("/user/balance".into(), None),
            Self::OpenRouter => ("/api/v1/key".into(), None),
            Self::Devin => {
                // Devin's billing day starts at 08:00 UTC, the year round.
                let start = (now - 8 * HOUR).div_euclid(DAY) * DAY + 8 * HOUR;
                let end = start + DAY;
                (format!("/v3/organizations/{scope}/consumption/daily?time_after={start}&time_before={end}"), Some(end))
            }
            Self::Copilot => (format!("/users/{scope}/settings/billing/ai_credit/usage"), None),
        }
    }

    const fn headers(self) -> &'static [&'static str] {
        match self {
            Self::Copilot => &["Accept: application/vnd.github+json", "X-GitHub-Api-Version: 2026-03-10"],
            _ => &["Accept: application/json"],
        }
    }

    /// Reads the provider's answer. None when it is not in the documented form.
    fn parse(self, body: &[u8], now: i64, until: Option<i64>) -> Option<Result<Reading, Failure>> {
        let reply = Value::parse(body)?;
        let reading = |figure, details, until, depleted| Reading { observed: now, figure, details, until, depleted };
        Some(Ok(match self {
            Self::DeepSeek => {
                let [available, infos] = reply.fields(["is_available", "balance_infos"]);
                let balances = infos?
                    .elements()?
                    .map(|info| {
                        let [currency, total, granted] = info.fields(["currency", "total_balance", "granted_balance"]);
                        let currency = currency?;
                        let currency = if currency.is("CNY") {
                            Currency::Cny
                        } else if currency.is("USD") {
                            Currency::Usd
                        } else {
                            return None;
                        };
                        Some((Unit::Money(currency), amount(total?)?, granted.and_then(amount)))
                    })
                    .collect::<Option<Vec<_>>>()?;
                let &(unit, total, granted) = balances.first()?;
                let figure = Figure { kind: Kind::Balance, unit, value: total };
                let granted =
                    granted.filter(|value| *value > 0.0).map(|value| Figure { kind: Kind::Granted, unit, value });
                let other = balances.get(1).map(|&(unit, value, _)| Figure { kind: Kind::Balance, unit, value });
                reading(figure, [granted, other], None, available.and_then(Value::boolean) == Some(false))
            }
            Self::OpenRouter => {
                let [data] = reply.fields(["data"]);
                let [left, used, daily, monthly] =
                    data?.fields(["limit_remaining", "usage", "usage_daily", "usage_monthly"]);
                let dollars = |kind, value: f64| Figure { kind, unit: Unit::Money(Currency::Usd), value };
                let used = used.and_then(amount)?;
                // A key without a limit reports null. One that has overspent reports less than nothing left.
                let figure = match left.filter(|left| !left.null()) {
                    Some(left) => dollars(Kind::LimitLeft, left.number()?.max(0.0)),
                    None => dollars(Kind::Used, used),
                };
                let detail = |kind, value: Option<Value<'_>>| {
                    value.and_then(amount).filter(|value| *value > 0.0).map(|value| dollars(kind, value))
                };
                reading(figure, [detail(Kind::UsedToday, daily), detail(Kind::UsedThisMonth, monthly)], None, false)
            }
            Self::Devin => {
                let [total] = reply.fields(["total_acus"]);
                let figure = Figure { kind: Kind::UsedToday, unit: Unit::Acus, value: amount(total?)? };
                reading(figure, [None, None], until, false)
            }
            Self::Copilot => {
                let [period, items] = reply.fields(["timePeriod", "usageItems"]);
                let [year, month] = period?.fields(["year", "month"]);
                let (year, month) = (year?.integer()?, month?.unsigned().filter(|month| (1..=12).contains(month))?);
                let month = i64::try_from(month).ok()?;
                let (start, end) = (month_start(year, month), month_start(year, month + 1));
                if !(start..end).contains(&now) {
                    return None;
                }
                let mut unit: Option<std::borrow::Cow<'_, str>> = None;
                let mut credits = 0.0;
                let mut any = false;
                for item in items?.elements()? {
                    let [kind, gross] = item.fields(["unitType", "grossQuantity"]);
                    let kind = kind?.string(64)?;
                    // Quantities in different units cannot be added into one figure.
                    if unit.as_ref().is_some_and(|unit| !unit.eq_ignore_ascii_case(&kind)) {
                        return None;
                    }
                    unit = Some(kind);
                    credits += amount(gross?)?;
                    any = true;
                }
                // An empty report is also what an account billed through an
                // organization gets, so it is not a figure of zero.
                if !any {
                    return Some(Err(Failure::Nothing));
                }
                let figure = Figure { kind: Kind::UsedThisMonth, unit: Unit::AiCredits, value: credits };
                reading(figure, [None, None], Some(end), false)
            }
        }))
    }
}

/// A count or a sum of money the provider wrote as a number or as decimal text.
fn amount(value: Value<'_>) -> Option<f64> {
    let number = value.number().or_else(|| value.string(32)?.parse::<f64>().ok())?;
    (number.is_finite() && number >= 0.0).then_some(number)
}

/// The first second of a month in UTC. Month 13 is January of the next year.
fn month_start(year: i64, month: i64) -> i64 {
    let (year, month) = if month > 12 { (year + 1, month - 12) } else { (year, month) };
    // Days from the civil date, counting the year from March so a leap day ends it.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era - 719_468) * DAY
}

/// Where requests go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Providers,
    /// One loopback port serving every provider under its id.
    Loopback(u16),
}

/// A key, and the organization or user to ask about when the key does not say.
struct Credentials {
    key: String,
    scope: Option<String>,
    /// Where the key was found, for doctor.
    from_environment: bool,
}

/// Everything a look needs besides the clock. It is copied into the worker,
/// so the environment is read once, when the pane opens.
#[derive(Clone)]
pub(super) struct Sources {
    environment: Vec<(&'static str, String)>,
    settings: Option<PathBuf>,
    cache: Option<PathBuf>,
    origin: Origin,
}

impl Sources {
    /// The user's own environment, settings file and state directory.
    pub(super) fn installed() -> Self {
        let names = UsageAccount::ALL.into_iter().flat_map(|account| [Some(account.key_name()), account.scope_name()]);
        let environment = names
            .flatten()
            .filter_map(|name| Some((name, std::env::var(name).ok().filter(|value| !value.is_empty())?)))
            .collect();
        let origin = std::env::var(TEST_PORT)
            .ok()
            .and_then(|port| port.parse().ok())
            .map_or(Origin::Providers, Origin::Loopback);
        Self {
            environment,
            settings: paths::config_dir().map(|dir| dir.join(SETTINGS_FILE)),
            cache: paths::state_dir().map(|dir| dir.join("accounts")),
            origin,
        }
    }

    #[cfg(test)]
    pub(super) fn test(environment: &[(&'static str, &str)], settings: PathBuf, cache: PathBuf, port: u16) -> Self {
        Self {
            environment: environment.iter().map(|&(name, value)| (name, value.to_owned())).collect(),
            settings: Some(settings),
            cache: Some(cache),
            origin: Origin::Loopback(port),
        }
    }

    /// The settings file's lines, if it is the user's own private file.
    fn settings(&self) -> Result<Option<String>, Failure> {
        let Some(path) = &self.settings else { return Ok(None) };
        let mut file = match fs_ops::open_read(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Failure::OpenFile),
        };
        let metadata = file.metadata().map_err(|_| Failure::OpenFile)?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let mine = metadata.uid() == unsafe { libc::geteuid() };
        if !metadata.is_file() || !mine || metadata.mode() & 0o077 != 0 || metadata.len() > SETTINGS_LIMIT {
            return Err(Failure::OpenFile);
        }
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(|_| Failure::OpenFile)?;
        Ok(Some(text))
    }

    /// The environment comes first, so one shell can use another key without editing the file.
    fn credentials(&self, account: UsageAccount) -> Result<Credentials, Failure> {
        let environment = |name: &str| {
            let named = self.environment.iter().find(|(known, _)| *known == name);
            named.map(|(_, value)| value.clone()).filter(|value| !value.is_empty())
        };
        let mut file = None;
        let mut saved = |name: &str| -> Result<Option<String>, Failure> {
            if file.is_none() {
                file = Some(self.settings()?.unwrap_or_default());
            }
            let lines = file.as_deref().unwrap_or_default().lines().map(str::trim);
            Ok(lines
                .filter(|line| !line.starts_with('#'))
                .filter_map(|line| line.split_once('='))
                .find(|(key, _)| key.trim() == name)
                .map(|(_, value)| value.trim().to_owned())
                .filter(|value| !value.is_empty()))
        };
        let (key, from_environment) = match environment(account.key_name()) {
            Some(key) => (key, true),
            None => (saved(account.key_name())?.ok_or(Failure::NoKey)?, false),
        };
        if !(8..=512).contains(&key.len()) || !http::quotable(&key) {
            return Err(Failure::BadSetting);
        }
        let scope = match account.scope_name() {
            None => None,
            Some(name) => {
                let scope = match environment(name) {
                    Some(scope) => scope,
                    None => saved(name)?.ok_or(Failure::NoScope)?,
                };
                if !account.valid_scope(&scope) {
                    return Err(Failure::BadSetting);
                }
                Some(scope)
            }
        };
        Ok(Credentials { key, scope, from_environment })
    }

    fn cache_file(&self, account: UsageAccount) -> Option<PathBuf> {
        Some(self.cache.as_ref()?.join(format!("{}.json", account.id())))
    }

    /// The last look any Milo process saved for this account and scope.
    fn remembered(&self, account: UsageAccount, scope: Option<&str>, now: i64) -> Option<Fetched> {
        let mut file = fs_ops::open_read(&self.cache_file(account)?).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > CACHE_LIMIT {
            return None;
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        let [version, saved_scope, asked, next, failure, reading] =
            Value::parse(&bytes)?.fields(["version", "scope", "asked", "next", "failure", "reading"]);
        let same_scope = match (scope, saved_scope) {
            (Some(scope), Some(saved)) => saved.is(scope),
            (None, None) => true,
            _ => false,
        };
        let asked = asked?.integer()?;
        // A look saved while the clock was ahead would put off every later one.
        if version?.unsigned() != Some(1) || !same_scope || asked > now + CLOCK_SKEW {
            return None;
        }
        Some(Fetched {
            reading: reading.and_then(Reading::saved).filter(|reading| reading.usable(now)),
            failure: failure.and_then(|saved| Failure::NAMES.iter().find(|(_, name, _)| saved.is(name)).map(|f| f.0)),
            asked,
            next: next?.integer()?,
        })
    }

    /// Saves the figures and times of a look for other panes and the menu.
    /// The key is never written, and a failed save only costs a request.
    fn remember(&self, account: UsageAccount, scope: Option<&str>, fetched: &Fetched) {
        let Some(path) = self.cache_file(account) else { return };
        let Some(dir) = path.parent() else { return };
        let mut text = String::with_capacity(256);
        text.push_str(r#"{"version":1"#);
        if let Some(scope) = scope {
            // A scope is letters, digits, hyphens and underscores, checked before any request.
            write!(text, r#","scope":"{scope}""#).expect("writing to a string");
        }
        write!(text, r#","asked":{},"next":{}"#, fetched.asked, fetched.next).expect("writing to a string");
        if let Some(failure) = fetched.failure {
            write!(text, r#","failure":"{}""#, failure.name()).expect("writing to a string");
        }
        if let Some(reading) = &fetched.reading {
            text.push_str(r#","reading":"#);
            reading.save(&mut text);
        }
        text.push_str("}\n");
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let temp = dir.join(format!(".{}.{}.tmp", account.id(), std::process::id()));
        let _ = std::fs::remove_file(&temp);
        let Ok((temp, mut file)) = fs_ops::Temp::create(temp) else { return };
        if file.write_all(text.as_bytes()).is_ok() {
            let _ = temp.replace(&path);
        }
    }

    /// One look. The saved answer is used while it is recent enough; otherwise
    /// the provider is asked once and the answer saved.
    fn fetch(&self, account: UsageAccount, now: i64, manual: bool) -> Fetched {
        let credentials = match self.credentials(account) {
            Ok(credentials) => credentials,
            // Nothing was sent. Look at the settings again soon, in case they change.
            Err(failure) => {
                return Fetched { reading: None, failure: Some(failure), asked: 0, next: now + MANUAL_REST };
            }
        };
        let scope = credentials.scope.as_deref();
        let before = self.remembered(account, scope, now);
        if let Some(saved) = before {
            // A reload may ask sooner than the schedule, but never inside a rate limit.
            let limited = if saved.failure == Some(Failure::Limited) { saved.next } else { 0 };
            let rest = if manual { limited.max(saved.asked + MANUAL_REST) } else { saved.next };
            if now < rest {
                return saved;
            }
        }
        let (path, until) = account.question(scope, now);
        let url = match self.origin {
            Origin::Providers => format!("https://{}{path}", account.host()),
            Origin::Loopback(port) => format!("http://127.0.0.1:{port}/{}{path}", account.id()),
        };
        let request = http::Request { url: &url, bearer: &credentials.key, headers: account.headers() };
        let interval = account.interval();
        let (answer, rest) = match http::get(&request, HTTP_WAIT) {
            None => (Err(Failure::Unreachable), interval),
            Some(reply) => match reply.status {
                200 => (account.parse(&reply.body, now, until).unwrap_or(Err(Failure::Unexpected)), interval),
                401 => (Err(Failure::Refused), REFUSED_REST),
                // GitHub reports its secondary rate limits as forbidden, with a wait.
                429 | 403 if reply.status == 429 || reply.retry_after.is_some() => {
                    let wait = reply.retry_after.and_then(|wait| i64::try_from(wait).ok()).unwrap_or(LIMITED_REST);
                    (Err(Failure::Limited), wait.clamp(MINUTE, HOUR))
                }
                403 | 404 => (Err(Failure::Denied), REFUSED_REST),
                _ => (Err(Failure::Unreachable), interval),
            },
        };
        let fetched = match answer {
            Ok(reading) => Fetched { reading: Some(reading), failure: None, asked: now, next: now + rest },
            Err(failure) => {
                // The last answer still describes the account when the provider merely did not give a new one.
                let earlier = before.and_then(|saved| saved.reading).filter(|_| failure != Failure::Nothing);
                Fetched { reading: earlier, failure: Some(failure), asked: now, next: now + rest }
            }
        };
        self.remember(account, scope, &fetched);
        fetched
    }
}

/// What the pane can say about an account right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Status {
    /// A reading, and its age in seconds once it is old enough to mention.
    Reading(Reading, Option<i64>),
    /// The first answer has not arrived.
    Checking,
    Failed(Failure),
    /// The last reading has aged out and no look has replaced it.
    Outdated,
}

pub(super) struct Account {
    pub(super) which: UsageAccount,
    pub(super) show: Show,
    fetched: Option<Fetched>,
    /// When this process should look again.
    next: i64,
    job: Option<sqlite::Job<Fetched>>,
}

impl Account {
    fn new(which: UsageAccount, show: Show) -> Self {
        Self { which, show, fetched: None, next: 0, job: None }
    }

    pub(super) fn status(&self, now: i64) -> Status {
        let Some(fetched) = &self.fetched else { return Status::Checking };
        let failure = fetched.failure;
        match fetched.reading.filter(|reading| reading.usable(now) && !failure.is_some_and(Failure::local)) {
            Some(reading) => {
                let age = now.saturating_sub(reading.observed);
                Status::Reading(reading, (age > self.which.fresh_for()).then_some(age))
            }
            None => failure.map_or(Status::Outdated, Status::Failed),
        }
    }

    /// An account the user turned on is in use here, with or without a figure.
    pub(super) fn presence(&self, now: i64) -> Presence {
        if matches!(self.status(now), Status::Reading(..)) { Presence::Reading } else { Presence::Found }
    }

    pub(super) fn checking(&self) -> bool {
        self.job.as_ref().is_some_and(|job| job.checking)
    }
}

/// The accounts the user turned on. A hidden account has no entry: its
/// settings are not read and its provider is never asked.
pub(super) struct Accounts {
    sources: Option<Sources>,
    accounts: Vec<Account>,
}

impl Accounts {
    pub(super) const fn none() -> Self {
        Self { sources: None, accounts: Vec::new() }
    }

    pub(super) fn open(choices: UsageAccounts) -> Self {
        let mut accounts = Self::none();
        accounts.choose(choices, Sources::installed);
        accounts
    }

    #[cfg(test)]
    pub(super) fn with(sources: Sources, choices: UsageAccounts) -> Self {
        let mut accounts = Self::none();
        accounts.choose(choices, || sources);
        accounts
    }

    /// Applies new choices, keeping what is known of an account that stays on.
    /// The environment and paths are read when the first account is turned on.
    pub(super) fn choose(&mut self, choices: UsageAccounts, sources: impl FnOnce() -> Sources) {
        let mut kept = std::mem::take(&mut self.accounts);
        for which in UsageAccount::ALL {
            let show = choices[which];
            if show == Show::Hide {
                continue;
            }
            let known = kept.iter().position(|account| account.which == which).map(|i| kept.swap_remove(i));
            self.accounts.push(Account { show, ..known.unwrap_or_else(|| Account::new(which, show)) });
        }
        if self.sources.is_none() && !self.accounts.is_empty() {
            self.sources = Some(sources());
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &Account> {
        self.accounts.iter()
    }

    /// Starts a look for each account that is due one, or for all on a deliberate reload.
    pub(super) fn start(&mut self, now: i64, manual: bool) {
        let Some(sources) = &self.sources else { return };
        for account in &mut self.accounts {
            if let Some(job) = &mut account.job {
                job.checking |= manual;
                continue;
            }
            if !manual && now < account.next {
                continue;
            }
            let (sources, which, first) = (sources.clone(), account.which, account.fetched.is_none());
            // If no thread can be started, the next check tries again.
            account.job = sqlite::Job::start(first || manual, move || Some(sources.fetch(which, now, manual))).ok();
        }
    }

    /// Takes in finished looks. True when one changed what the pane shows or
    /// ended a look it was announcing.
    pub(super) fn collect(&mut self, now: i64) -> bool {
        let mut changed = false;
        for account in &mut self.accounts {
            let Some(result) = account.job.as_ref().and_then(sqlite::Job::finished) else { continue };
            let announced = account.job.take().is_some_and(|job| job.checking);
            // A worker that died leaves what is known, and the look is tried again later.
            account.next = result.map_or(now + MANUAL_REST, |fetched| fetched.next.max(now + MANUAL_REST));
            if let Some(fetched) = result {
                changed |= account.fetched != Some(fetched);
                account.fetched = Some(fetched);
            }
            changed |= announced;
        }
        changed
    }

    pub(super) fn fd(&self) -> Option<RawFd> {
        self.accounts.iter().find_map(|account| account.job.as_ref().map(sqlite::Job::fd))
    }

    /// Whether a look is still running.
    pub(super) fn pending(&self) -> bool {
        self.accounts.iter().any(|account| account.job.is_some())
    }

    pub(super) fn checking(&self) -> bool {
        self.accounts.iter().any(Account::checking)
    }
}

/// What doctor says about each account that is turned on, from the settings
/// and the last saved answer. It does not ask the provider.
pub(super) fn doctor(choices: UsageAccounts, now: i64) -> Vec<String> {
    if !choices.any() {
        return Vec::new();
    }
    Sources::installed().doctor(choices, now)
}

impl Sources {
    pub(super) fn doctor(&self, choices: UsageAccounts, now: i64) -> Vec<String> {
        let file = self.settings.as_deref().map(Path::display);
        let file = file.map_or_else(|| SETTINGS_FILE.to_owned(), |file| file.to_string());
        let mut lines = Vec::new();
        let curl = Path::new(http::executable());
        if choices.any() && !curl.exists() {
            lines.push(format!("Usage accounts: {} is missing, so no provider can be asked", curl.display()));
        }
        for account in UsageAccount::ALL.into_iter().filter(|account| choices[*account] != Show::Hide) {
            let title = format!("{} {}", account.label(), account.subject());
            let credentials = match self.credentials(account) {
                Ok(credentials) => credentials,
                Err(failure) => {
                    let name = match failure {
                        Failure::NoScope => account.scope_name().unwrap_or_default(),
                        _ => account.key_name(),
                    };
                    lines.push(match failure {
                        Failure::NoKey | Failure::NoScope => {
                            format!("{title}: not asked; set {name} in the environment or in {file}")
                        }
                        Failure::OpenFile => {
                            format!("{title}: not asked; {file} must be a plain file only you can read (chmod 600)")
                        }
                        _ => {
                            format!("{title}: not asked; {name} or the account it names has a form Milo will not send")
                        }
                    });
                    continue;
                }
            };
            let place = if credentials.from_environment { "the environment" } else { file.as_str() };
            let every = account.interval() / MINUTE;
            lines.push(format!(
                "{title}: asks {} with {} from {place}, at most every {every} minutes",
                account.host(),
                account.key_name()
            ));
            let Some(saved) = self.remembered(account, credentials.scope.as_deref(), now) else {
                lines.push("  no answer saved yet".into());
                continue;
            };
            if let Some(reading) = saved.reading {
                let mut age = when::duration(now.saturating_sub(reading.observed).max(0), Detail::Rounded);
                age.retain(|c| c != ' ');
                lines.push(format!("  last answer: {}, {age} ago", reading.figure.text()));
            }
            if let Some(failure) = saved.failure {
                lines.push(format!("  last attempt: {}", failure.words()));
            }
        }
        lines
    }
}

#[cfg(test)]
impl Accounts {
    /// Runs every due look to its end, as the pane's loop would over time.
    pub(super) fn settle(&mut self, now: i64, manual: bool) {
        self.start(now, manual);
        while let Some(fd) = self.fd() {
            let mut poll = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: `poll` points to one writable pollfd whose descriptor the job keeps open.
            unsafe { libc::poll(&raw mut poll, 1, 30_000) };
            self.collect(now);
        }
    }

    pub(super) fn set(&mut self, which: UsageAccount, reading: Option<Reading>, failure: Option<Failure>, now: i64) {
        let account = self.accounts.iter_mut().find(|account| account.which == which).expect("the account is on");
        account.fetched = Some(Fetched { reading, failure, asked: now, next: i64::MAX });
        account.next = i64::MAX;
    }
}
