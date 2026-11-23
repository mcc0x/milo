//! Streams the same local readings as the pane without opening a terminal.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::menubar::Indicator;
use crate::pane::Pane as _;
use crate::preferences::{Preferences, UsageAccounts};
use crate::{paths, term, when};

use super::{
    Activity, Reading, SAVED_LIMITS, Show, Source, Usage, UsageTool, UsageTools, Window, accounts, claude_sources,
    codex_sessions, draw, omp, opencode, sqlite,
};

const INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Refresh {
    Start,
    Scheduled,
    Manual { indicator_ack: bool },
    Indicator,
}

impl Refresh {
    pub(super) fn acknowledges_indicator(self) -> bool {
        matches!(self, Self::Indicator | Self::Manual { indicator_ack: true })
    }
}

pub fn menu_stream() -> io::Result<()> {
    let home = paths::home();
    let home = home.as_deref();
    let config = paths::config_dir();
    let config = config.as_deref();
    let indicator_file = config.map(Indicator::file);
    let (tools, chosen) = global_choices(config).unwrap_or_default();
    let mut usage = Usage::for_home(home, when::now(), tools).with_databases(
        omp::database(home),
        opencode::database(home),
        sqlite::QUERY_WAIT,
    );
    usage.accounts = accounts::Accounts::open(chosen);
    // Every frame uses quota if the saved choice cannot be read, never a stale resource mode.
    let load_indicator = || indicator_file.as_deref().and_then(|file| Indicator::load(file).ok()).unwrap_or_default();
    snapshots(io::stdin(), io::stdout().lock(), |line, request| {
        let indicator_ack = request.acknowledges_indicator();
        if request == Refresh::Indicator {
            // A display choice must not force provider lookups or postpone their next refresh.
            usage.menu_snapshot(line, load_indicator(), indicator_ack);
            return Ok(None);
        }
        if request != Refresh::Start
            && let Some((tools, chosen)) = global_choices(config)
        {
            usage.apply_menu_tools(home, tools);
            usage.accounts.choose(chosen, accounts::Sources::installed);
        }
        let settled = usage.refresh(usage.now(), Instant::now(), matches!(request, Refresh::Manual { .. }));
        // The pane repaints when its SQLite jobs finish. Here a complete refresh
        // is one line, so collect both bounded reads before publishing it.
        while let Some(fd) = usage.database_fd() {
            ready(&mut [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }], None)?;
            usage.check_external();
        }
        // Publish reset and freshness changes on the same boundary as the pane,
        // even if the usual poll is later.
        // A busy transcript can span several bounded reads. Don't wait a full
        // idle interval between batches, or a partial daily total lingers.
        // A provider can take seconds to answer, so its figure follows in a
        // later line rather than holding this one back.
        let waiting = !settled || usage.accounts.pending();
        let interval = if waiting { Duration::from_secs(1) } else { INTERVAL };
        let wait = interval.min(usage.next_frame().unwrap_or(interval));
        usage.menu_snapshot(line, load_indicator(), indicator_ack);
        Ok(Some(wait))
    })
}

/// Reload the global layer only. Keep its last valid visibility if an edit is temporarily invalid.
pub(super) fn global_choices(dir: Option<&Path>) -> Option<(UsageTools, UsageAccounts)> {
    let Some(dir) = dir else { return Some(Default::default()) };
    Preferences::resolve_in(dir, None).ok().map(|(prefs, _)| (prefs.usage_tools, prefs.usage_accounts))
}

impl Usage {
    /// Always/Auto only affect presentation. A Hide transition resets just the
    /// affected reader, without losing the other providers' incremental tails.
    pub(super) fn apply_menu_tools(&mut self, home: Option<&Path>, tools: UsageTools) {
        let before = self.tools;
        let hidden = |tool| tools[tool] == Show::Hide;
        let changed = |tool| (before[tool] == Show::Hide) != hidden(tool);
        self.tools = tools;
        if changed(UsageTool::Codex) {
            self.codex = Default::default();
            self.codex_dir = if hidden(UsageTool::Codex) { None } else { codex_sessions(home) };
        }
        if changed(UsageTool::Claude) {
            self.claude = Default::default();
            let (projects, config, state) = if hidden(UsageTool::Claude) { (None, None, None) } else { claude_sources() };
            self.claude_dir = projects;
            self.claude_config = config;
            self.limits_file = state.map(|dir| dir.join(SAVED_LIMITS));
        }
        if changed(UsageTool::Codex) || changed(UsageTool::Claude) {
            let path = self.reads_omp().then(|| omp::database(home)).flatten();
            self.omp = omp::poll(path, sqlite::QUERY_WAIT);
        }
        if changed(UsageTool::OpenCode) {
            let path = (!hidden(UsageTool::OpenCode)).then(|| opencode::database(home)).flatten();
            self.opencode = super::OpenCode::new(path, sqlite::QUERY_WAIT);
        }
    }

    pub(super) fn menu_snapshot(&mut self, output: &mut String, indicator: Indicator, indicator_ack: bool) {
        let now = self.now();
        let today = self.today.since(now);
        let codex = self.codex.reported(now);
        let claude = self.claude.reported(now);
        let presence = [self.codex.presence(codex), self.claude.presence(claude), self.opencode.presence(today)];
        write!(output, r#"{{"version":1,"indicator":"{}","#, indicator.value()).unwrap();
        if indicator_ack {
            output.push_str(r#""indicatorAck":true,"#);
        }
        output.push_str(r#""providers":["#);
        let mut separator = "";
        for (tool, presence) in UsageTool::ALL.into_iter().zip(presence) {
            if !presence.placed(self.tools[tool]) {
                continue;
            }
            let id = match tool {
                UsageTool::Codex => "codex",
                UsageTool::Claude => "claude",
                UsageTool::OpenCode => "opencode",
            };
            write!(output, r#"{separator}{{"id":"{id}","name":"{}","windows":["#, tool.label()).unwrap();
            separator = ",";
            let (state, activity) = match tool {
                UsageTool::Codex => (
                    limit_windows(output, codex, now, || self.limit_hint(tool, codex.is_some(), now).text()),
                    None,
                ),
                UsageTool::Claude => (
                    limit_windows(output, claude, now, || self.limit_hint(tool, claude.is_some(), now).text()),
                    draw::activity_text(self.claude.activity(today, now)),
                ),
                UsageTool::OpenCode => self.opencode.menu_detail(today),
            };
            output.push(']');
            if let Some(state) = state {
                // These fixed captions are ASCII. Start the menu's standalone
                // detail with a capital without allocating another string.
                let initial = char::from(state.as_bytes()[0].to_ascii_uppercase());
                write!(output, r#","detail":"{initial}{}"#, &state[1..]).unwrap();
                if let Some(activity) = &activity {
                    write!(output, " · {activity}").unwrap();
                }
                output.push('"');
            } else if let Some(activity) = activity {
                write!(output, r#","detail":"{activity}""#).unwrap();
            }
            output.push('}');
        }
        // An account has no windows: money, credits and compute units are
        // written out, and never offered to the badge as a share of a limit.
        for account in self.accounts.iter() {
            if !account.presence(now).placed(account.show) {
                continue;
            }
            let detail = draw::account_text(account.status(now));
            let (id, name) = (account.which.id(), account.which.label());
            write!(output, r#"{separator}{{"id":"{id}","name":"{name}","windows":[],"detail":"{detail}"}}"#).unwrap();
            separator = ",";
        }
        output.push_str("]}\n");
    }
}

impl super::OpenCode {
    fn menu_detail(&self, today: i64) -> (Option<&'static str>, Option<String>) {
        if let Some(activity) = self.today(today) {
            let mut detail = draw::activity_text(Activity { today: activity.tokens.fresh(), partial: false })
                .expect("today's OpenCode activity has tokens");
            if let Some(cost) = activity.cost.and_then(opencode::Cost::text) {
                write!(detail, " · {cost} est.").unwrap();
            }
            return (None, Some(detail));
        }
        (Some(self.activity_hint(today).text()), None)
    }
}

/// Figures on elapsed windows are suppressed in the pane too. Stale but
/// unelapsed readings can still explain what was last reported, not drive a badge.
fn limit_windows(
    output: &mut String,
    report: Option<(&Reading, Source)>,
    now: i64,
    missing: impl FnOnce() -> &'static str,
) -> Option<&'static str> {
    let Some((reading, source)) = report.filter(|(reading, _)| reading.windows.any()) else {
        return Some(missing());
    };
    let mut separator = "";
    let mut emit = |window: Window, label: &str| {
        // Base labels are generated from a duration, and model captions allow
        // only ASCII names. Provider names and detail are fixed or numeric text.
        write!(
            output,
            r#"{separator}{{"label":"{label}","remaining":{},"fresh":{},"staleAt":{}"#,
            window.remaining(),
            !window.is_stale(now, source),
            window.stale_at(source),
        )
        .unwrap();
        if let Some(reset) = window.resets {
            write!(output, r#","resetAt":{reset}"#).unwrap();
        }
        output.push('}');
        separator = ",";
    };
    for window in reading.windows.base.into_iter().flatten().filter(|w| !w.is_reset(now)) {
        emit(window, &window.label());
    }
    if let Some(model) = reading.windows.model_week.as_ref().filter(|model| !model.window.is_reset(now)) {
        emit(model.window, model.caption());
    }
    separator.is_empty().then_some("reset elapsed")
}

pub(super) fn snapshots(
    input: impl AsFd,
    mut output: impl Write + AsFd,
    mut refresh: impl FnMut(&mut String, Refresh) -> io::Result<Option<Duration>>,
) -> io::Result<()> {
    let mut line = String::with_capacity(1024);
    let mut request = Refresh::Start;
    let mut deadline = Instant::now();
    loop {
        line.clear();
        if let Some(interval) = refresh(&mut line, request)? {
            deadline = Instant::now() + interval;
        }
        match output.write_all(line.as_bytes()).and_then(|()| output.flush()) {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            result => result?,
        }
        match next_refresh(input.as_fd().as_raw_fd(), output.as_fd().as_raw_fd(), deadline)? {
            Some(next) => request = next,
            None => return Ok(()),
        }
    }
}

/// A newline refreshes usage; NUL only rereads the display choice. Coalesce queued requests.
fn next_refresh(input: RawFd, output: RawFd, deadline: Instant) -> io::Result<Option<Refresh>> {
    let mut fds = [
        libc::pollfd { fd: input, events: libc::POLLIN, revents: 0 },
        // Darwin requires POLLHUP in the mask to report a closed pipe reader.
        libc::pollfd { fd: output, events: libc::POLLHUP, revents: 0 },
    ];
    let mut buffer = [0u8; 256];
    loop {
        if ready(&mut fds, Some(deadline))? == 0 {
            return Ok(Some(Refresh::Scheduled));
        }
        if fds[1].revents != 0 {
            return Ok(None);
        }
        // SAFETY: The input stays open for the entire stream, this is its only
        // reader, and poll reported readiness. The buffer is writable.
        match unsafe { libc::read(input, buffer.as_mut_ptr().cast(), buffer.len()) } {
            -1 => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            0 => return Ok(None),
            n => {
                let bytes = &buffer[..usize::try_from(n).unwrap()];
                let indicator_ack = bytes.contains(&0);
                if bytes.contains(&b'\n') {
                    return Ok(Some(Refresh::Manual { indicator_ack }));
                }
                if indicator_ack {
                    return Ok(Some(Refresh::Indicator));
                }
            }
        }
        if Instant::now() >= deadline {
            return Ok(Some(Refresh::Scheduled));
        }
    }
}

fn ready(fds: &mut [libc::pollfd], deadline: Option<Instant>) -> io::Result<i32> {
    loop {
        let timeout = deadline.map_or(-1, |at| term::milliseconds(at.saturating_duration_since(Instant::now())));
        let result = term::wait_ready(fds, timeout);
        if result >= 0 {
            return Ok(result);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
