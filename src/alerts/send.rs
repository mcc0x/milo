//! Posts one message with `/usr/bin/curl`, the only program Milo sends with.
//!
//! The address holds the destination's secret, so the request is given to
//! curl on its standard input, never on its command line. curl may only
//! speak HTTPS, follows no redirect, and reads no settings file of the user's.

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use super::Destination;
use crate::json::Value;

const CURL: &str = "/usr/bin/curl";
const ANSWER_LIMIT: u64 = 1 << 16;
/// How long to wait when a destination limits the rate without saying for how long.
const DEFAULT_WAIT: Duration = Duration::from_secs(5);

/// How a destination answered.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Sent,
    /// The destination limits the rate and says when to try again.
    Wait(Duration),
    /// The destination will not take this request, now or later: a wrong or
    /// revoked secret, an unknown chat, or a bot that may not post there.
    Refused(u16),
    /// The destination could not be reached, or failed. Trying again may work.
    Failed(Option<u16>),
}

impl Outcome {
    /// Why a message was not delivered, for the user.
    pub(crate) fn reason(&self, destination: Destination) -> String {
        let name = destination.title();
        match self {
            Self::Sent => "it was delivered".into(),
            Self::Wait(wait) => format!("{name} is limiting the rate; it asks to wait {} seconds", wait.as_secs()),
            Self::Refused(code @ (401 | 403 | 404)) => {
                format!("{name} refused the saved secret or destination (HTTP {code}); save it again")
            }
            Self::Refused(code) => format!("{name} refused the request (HTTP {code}); check the chat or channel"),
            Self::Failed(Some(code)) => format!("{name} answered with a server error (HTTP {code})"),
            Self::Failed(None) => format!("{name} could not be reached"),
        }
    }
}

/// Posts `body` as JSON to `address` and reports the answer. While curl
/// runs, its process ID is in `running`, for a signal handler to end it.
pub(crate) fn post(address: &str, body: &str, running: Option<&AtomicI32>) -> Outcome {
    let Some(answer) = run(&request(address, body), running) else { return Outcome::Failed(None) };
    // The status follows the answer's body on a line of its own.
    let (body, code) = answer.rsplit_once('\n').unwrap_or(("", &answer));
    match code.trim().parse::<u16>() {
        Ok(200..=299) => Outcome::Sent,
        Ok(429) => Outcome::Wait(retry_after(body)),
        Ok(code @ 400..=499) => Outcome::Refused(code),
        Ok(code @ 500..=599) => Outcome::Failed(Some(code)),
        _ => Outcome::Failed(None),
    }
}

/// curl's options, in the form of its own settings file.
pub(super) fn request(address: &str, body: &str) -> String {
    // Only a test's stand-in on this machine is spoken to without TLS.
    let protocol = if address.starts_with("http://127.0.0.1:") { "http" } else { "https" };
    format!(
        "url = {}\nrequest = \"POST\"\nheader = \"Content-Type: application/json\"\ndata-raw = {}\n\
         proto = \"={protocol}\"\nmax-redirs = 0\nnoproxy = \"127.0.0.1\"\nconnect-timeout = 8\nmax-time = 15\n\
         silent\nwrite-out = \"\\n%{{http_code}}\"\n",
        quoted(address),
        quoted(body)
    )
}

/// A value between double quotes, as curl's settings files read one.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' | '\\' => out.extend(['\\', c]),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn run(request: &str, running: Option<&AtomicI32>) -> Option<String> {
    // `-q` must come first: it keeps curl from reading the user's own curlrc.
    let mut curl = Command::new(CURL)
        .args(["-q", "--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(running) = running {
        running.store(curl.id().try_into().unwrap_or(0), Ordering::Relaxed);
    }
    let written = curl.stdin.take().map(|mut input| input.write_all(request.as_bytes()));
    let mut answer = String::new();
    let read = curl.stdout.take().map(|output| output.take(ANSWER_LIMIT).read_to_string(&mut answer));
    // curl has closed its output, so it has ended or is about to: from here
    // on its ID may no longer be signalled.
    if let Some(running) = running {
        running.store(0, Ordering::Relaxed);
    }
    let status = curl.wait();
    // An HTTP error is still an answer; curl itself fails only when there is none.
    (matches!(written, Some(Ok(()))) && matches!(read, Some(Ok(_))) && status.ok()?.success()).then_some(answer)
}

/// Telegram gives whole seconds in `parameters.retry_after`; Discord gives
/// seconds with a fraction in `retry_after`.
pub(super) fn retry_after(body: &str) -> Duration {
    let Some(answer) = Value::parse(body.trim().as_bytes()) else { return DEFAULT_WAIT };
    let [direct, parameters] = answer.fields(["retry_after", "parameters"]);
    let nested = parameters.and_then(|parameters| parameters.fields(["retry_after"])[0]);
    match direct.or(nested).and_then(Value::number) {
        Some(seconds) if (0.0..=86_400.0).contains(&seconds) => Duration::from_secs_f64(seconds),
        _ => DEFAULT_WAIT,
    }
}
