//! Streams the same local readings as the pane without opening a terminal.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::pane::Pane as _;
use crate::preferences::Preferences;
use crate::{paths, term, when};

use super::{
    Activity, CodexSessions, Reading, SAVED_LIMITS, Show, Source, Usage, UsageTool, UsageTools, Window,
    claude_sources, codex_sessions, draw, omp, opencode, sqlite,
};

const INTERVAL: Duration = Duration::from_secs(30);

pub fn menu_stream() -> io::Result<()> {
    let home = paths::home();
    let home = home.as_deref();
    let config = paths::config_dir();
    let config = config.as_deref();
    let mut usage = Usage::for_home(home, when::now(), global_tools(config).unwrap_or_default())
        .with_databases(omp::database(home), opencode::database(home), sqlite::QUERY_WAIT);
    snapshots(io::stdin(), io::stdout().lock(), |line, request| {
        if request.is_some() && let Some(tools) = global_tools(config) {
            usage.apply_menu_tools(home, tools);
        }
        let settled = usage.refresh(usage.now(), Instant::now(), request.unwrap_or(false));
        // The pane repaints when its SQLite jobs finish. Here a complete refresh
        // is one line, so collect both bounded reads before publishing it.
        while let Some(fd) = usage.external_fd() {
            ready(&mut [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }], None)?;
            usage.check_external();
        }
        // Publish reset and freshness changes on the same boundary as the pane,
        // even if the usual poll is later.
        // A busy transcript can span several bounded reads. Don't wait a full
        // idle interval between batches, or a partial daily total lingers.
        let interval = if settled { INTERVAL } else { Duration::from_secs(1) };
        let wait = interval.min(usage.next_frame().unwrap_or(interval));
        usage.menu_snapshot(line);
        Ok(wait)
    })
}

/// Reload the global layer only. Keep its last valid visibility if an edit is temporarily invalid.
pub(super) fn global_tools(dir: Option<&Path>) -> Option<UsageTools> {
    let Some(dir) = dir else { return Some(UsageTools::default()) };
    Preferences::resolve_in(dir, None).ok().map(|(prefs, _)| prefs.usage_tools)
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

    pub(super) fn menu_snapshot(&mut self, output: &mut String) {
        let now = self.now();
        let today = self.today.since(now);
        let codex = self.codex.reported(now);
        let claude = self.claude.reported(now);
        let presence = [self.codex.presence(codex), self.claude.presence(claude), self.opencode.presence(today)];
        output.push_str(r#"{"version":1,"providers":["#);
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
                UsageTool::Codex => {
                    let missing = match self.codex.sessions {
                        CodexSessions::Missing => "No sessions found",
                        CodexSessions::Present => "No limits to show",
                        CodexSessions::Unreadable => "Could not read sessions",
                    };
                    (limit_windows(output, codex, now, missing), None)
                }
                UsageTool::Claude => (
                    limit_windows(output, claude, now, "No saved limits found"),
                    draw::activity_text(self.claude.activity(today, now)),
                ),
                UsageTool::OpenCode => self.opencode.menu_detail(today),
            };
            output.push(']');
            match (state, activity.as_deref()) {
                (Some(state), Some(activity)) => write!(output, r#","detail":"{state} · {activity}""#).unwrap(),
                (Some(detail), None) | (None, Some(detail)) => write!(output, r#","detail":"{detail}""#).unwrap(),
                (None, None) => {}
            }
            output.push('}');
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
        let state = match self.report {
            opencode::Report::Missing => "No database found",
            opencode::Report::Unsupported => "Database format not checked",
            opencode::Report::Unreadable => "Could not read its database",
            opencode::Report::Today(activity) if activity.since != today => "Checking local activity",
            opencode::Report::Today(_) => "No tokens recorded today",
        };
        (Some(state), None)
    }
}

/// Figures on elapsed windows are suppressed in the pane too. Stale but
/// unelapsed readings can still explain what was last reported, not drive a badge.
fn limit_windows(
    output: &mut String,
    report: Option<(&Reading, Source)>,
    now: i64,
    missing: &'static str,
) -> Option<&'static str> {
    let Some((reading, source)) = report else { return Some(missing) };
    let mut separator = "";
    let mut emit = |window: Window, label: &str| {
        // Base labels are generated from a duration, and model captions allow
        // only ASCII names. Provider names and detail are fixed or numeric text.
        write!(
            output,
            r#"{separator}{{"label":"{label}","remaining":{},"fresh":{}"#,
            window.remaining(),
            !window.is_stale(now, source),
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
    separator.is_empty().then(|| if reading.windows.any() { "Reset elapsed" } else { "No limits to show" })
}

pub(super) fn snapshots(
    input: impl AsFd,
    mut output: impl Write + AsFd,
    mut refresh: impl FnMut(&mut String, Option<bool>) -> io::Result<Duration>,
) -> io::Result<()> {
    let mut line = String::with_capacity(1024);
    let mut request = None;
    loop {
        line.clear();
        let interval = refresh(&mut line, request)?;
        match output.write_all(line.as_bytes()).and_then(|()| output.flush()) {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            result => result?,
        }
        match next_refresh(input.as_fd().as_raw_fd(), output.as_fd().as_raw_fd(), Instant::now() + interval)? {
            Some(manual) => request = Some(manual),
            None => return Ok(()),
        }
    }
}

/// Idle until the cadence or a newline; multiple pending requests can share one refresh.
fn next_refresh(input: RawFd, output: RawFd, deadline: Instant) -> io::Result<Option<bool>> {
    let mut fds = [
        libc::pollfd { fd: input, events: libc::POLLIN, revents: 0 },
        // Darwin requires POLLHUP in the mask to report a closed pipe reader.
        libc::pollfd { fd: output, events: libc::POLLHUP, revents: 0 },
    ];
    let mut buffer = [0u8; 256];
    loop {
        if ready(&mut fds, Some(deadline))? == 0 {
            return Ok(Some(false));
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
            n if buffer[..usize::try_from(n).unwrap()].contains(&b'\n') => return Ok(Some(true)),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Ok(Some(false));
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
