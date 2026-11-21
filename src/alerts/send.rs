//! Posts one message with `/usr/bin/curl`, the only program Milo sends with.
//!
//! Each request holds a secret, so it is given to curl on standard input,
//! never on its command line. curl follows no redirect and reads no user
//! settings file; only the loopback relay and test stand-ins use plain HTTP.

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
    /// The relay is temporarily unavailable and has not forwarded the message.
    Unavailable(Duration),
    /// The destination will not take this request, now or later: a wrong or
    /// revoked secret, an unknown chat, or a bot that may not post there.
    Refused(u16),
    /// The relay may have forwarded the message, but delivery was not confirmed.
    Uncertain(Option<u16>),
    /// The destination could not be reached, or failed. Trying again may work.
    Failed(Option<u16>),
}

impl Outcome {
    /// Why a message was not delivered, for the user.
    pub(super) fn reason(&self, destination: Destination, hosted_telegram: bool) -> String {
        let name = destination.title();
        match self {
            Self::Sent => "it was delivered".into(),
            Self::Wait(wait) => format!("{name} is limiting the rate; it asks to wait {} seconds", wait.as_secs()),
            Self::Unavailable(wait) => {
                format!("the Telegram relay is not ready; it asks to wait {} seconds", wait.as_secs())
            }
            Self::Refused(401) if hosted_telegram => "Telegram pairing expired or was revoked; reconnect with Milo".into(),
            Self::Refused(422) if hosted_telegram => "Telegram rejected this message".into(),
            Self::Refused(code @ (401 | 403 | 404)) => {
                format!("{name} refused the saved secret or destination (HTTP {code}); save it again")
            }
            Self::Refused(code) => format!("{name} refused the request (HTTP {code}); check the chat or channel"),
            Self::Uncertain(Some(code)) => format!("{name} delivery could not be confirmed (HTTP {code})"),
            Self::Uncertain(None) => format!("{name} delivery could not be confirmed"),
            Self::Failed(Some(code)) => format!("{name} answered with a server error (HTTP {code})"),
            Self::Failed(None) => format!("{name} could not be reached"),
        }
    }
}

/// Sends a prepared request and reports the answer. While curl runs, its
/// process ID is in `running`, for a signal handler to end it.
pub(super) fn post(request: &Request, running: Option<&AtomicI32>) -> Outcome {
    let Some((body, code)) = response(request, ANSWER_LIMIT, running) else {
        return if request.relay { Outcome::Uncertain(None) } else { Outcome::Failed(None) };
    };
    match code {
        204 if request.relay && body.is_empty() => Outcome::Sent,
        200..=299 if !request.relay => Outcome::Sent,
        429 => Outcome::Wait(retry_after(&body)),
        503 if request.relay => relay_retry_after(&body).map_or(Outcome::Uncertain(Some(code)), Outcome::Unavailable),
        400..=499 => Outcome::Refused(code),
        _ if request.relay => Outcome::Uncertain(Some(code)),
        500..=599 => Outcome::Failed(Some(code)),
        _ => Outcome::Failed(None),
    }
}

/// Posts a query and returns only a successful answer, bounded by `limit` bytes.
pub(super) fn query(address: &str, body: &str, limit: u64) -> Option<String> {
    let (body, code) = response(&request(address, body), limit, None)?;
    (200..=299).contains(&code).then_some(body)
}

pub(super) fn response(request: &Request, limit: u64, running: Option<&AtomicI32>) -> Option<(String, u16)> {
    let mut answer = run(request, limit, running)?;
    // The status follows the answer's body on a line of its own.
    let (body, code) = answer.rsplit_once('\n')?;
    let code = code.trim().parse().ok()?;
    answer.truncate(body.len());
    Some((answer, code))
}

/// curl's options, in the form of its own settings file.
pub(super) struct Request {
    pub(super) options: String,
    relay: bool,
}

pub(super) fn request(address: &str, body: &str) -> Request {
    config(address, "POST", Some(body), None, false, 8, 15)
}

pub(super) fn relay_request(address: &str, method: &str, body: Option<&str>, capability: Option<&str>) -> Request {
    config(address, method, body, capability, true, 2, if capability.is_some() && body.is_some() { 15 } else { 4 })
}

fn config(
    address: &str,
    method: &str,
    body: Option<&str>,
    capability: Option<&str>,
    relay: bool,
    connect_timeout: u8,
    timeout: u8,
) -> Request {
    use std::fmt::Write as _;
    let protocol = if address.starts_with("http://127.0.0.1:") { "http" } else { "https" };
    let mut request = String::with_capacity(256 + address.len() + body.map_or(0, str::len));
    request.push_str("url = ");
    push_quoted(&mut request, address);
    let _ = write!(request, "\nrequest = \"{method}\"\n");
    if let Some(body) = body {
        request.push_str("header = \"Content-Type: application/json\"\ndata-raw = ");
        push_quoted(&mut request, body);
        request.push('\n');
    }
    if let Some(capability) = capability {
        let _ = writeln!(request, "header = \"Authorization: Bearer {capability}\"");
    }
    let noproxy = if relay { "*" } else { "127.0.0.1" };
    if relay && protocol == "https" {
        request.push_str("tlsv1.2\n");
    }
    let _ = write!(
        request,
        "globoff\nproto = \"={protocol}\"\nmax-redirs = 0\nnoproxy = \"{noproxy}\"\nconnect-timeout = {connect_timeout}\n\
         max-time = {timeout}\nsilent\nwrite-out = \"\\n%{{http_code}}\"\n",
    );
    Request { options: request, relay }
}

/// A value between double quotes, as curl's settings files read one.
fn push_quoted(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' | '\\' => out.extend(['\\', c]),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn run(request: &Request, limit: u64, running: Option<&AtomicI32>) -> Option<String> {
    let mut command = Command::new(CURL);
    if request.relay {
        // Don't inherit proxy, custom CA, or debug/traffic-logging settings.
        command.env_clear();
    }
    // `-q` must come first: it keeps curl from reading the user's own curlrc.
    let mut curl = command
        .args(["-q", "--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(running) = running {
        running.store(curl.id().try_into().unwrap_or(0), Ordering::Relaxed);
    }
    let written = curl.stdin.take().map(|mut input| input.write_all(request.options.as_bytes()));
    let mut answer = String::new();
    let read = curl.stdout.take().map(|output| output.take(limit + 1).read_to_string(&mut answer));
    let over_limit = answer.len() as u64 > limit;
    if over_limit {
        let _ = curl.kill();
    }
    // The output was closed; from here the process ID may no longer be signalled.
    if let Some(running) = running {
        running.store(0, Ordering::Relaxed);
    }
    let status = curl.wait();
    // An HTTP error is still an answer; curl itself fails only when there is none.
    (!over_limit && matches!(written, Some(Ok(()))) && matches!(read, Some(Ok(_))) && status.ok()?.success())
        .then_some(answer)
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

/// A 503 without a valid hint could come from a gateway after the send.
fn relay_retry_after(body: &str) -> Option<Duration> {
    let seconds = Value::parse(body.trim().as_bytes())?.fields(["retry_after"])[0]?.unsigned()?;
    (1..=86_400).contains(&seconds).then(|| Duration::from_secs(seconds))
}
