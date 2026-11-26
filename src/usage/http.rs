//! Asks a provider one bounded question through the system's curl.
//!
//! The credential travels on curl's standard input, never in its arguments or
//! environment, so other processes cannot read it from the process table.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::sqlite;

/// A reply larger than this is not one of the small reports Milo asks for.
const MAX_REPLY: usize = 256 * 1024;

#[cfg(unix)]
pub(super) fn executable() -> PathBuf {
    PathBuf::from("/usr/bin/curl")
}

/// The curl that Windows itself ships, never one a project or \`PATH\` supplies.
#[cfg(windows)]
pub(super) fn executable() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).filter(|root| root.is_absolute());
    root.unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("System32").join("curl.exe")
}

/// One GET. `url` and `headers` are built by Milo; only `bearer` comes from the user.
pub(super) struct Request<'a> {
    pub(super) url: &'a str,
    pub(super) bearer: &'a str,
    pub(super) headers: &'a [&'a str],
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) retry_after: Option<RetryAfter>,
    pub(super) body: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RetryAfter {
    Seconds(i64),
    At(i64),
}

impl RetryAfter {
    fn parse(value: &str) -> Option<Self> {
        if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
            return Some(Self::Seconds(value.parse().unwrap_or(i64::MAX)));
        }
        super::time::http_date(value, crate::when::now()).map(Self::At)
    }
}

/// Whether `text` can sit inside a quoted value of curl's configuration
/// without ending the value or starting an escape.
pub(super) fn quotable(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\')
}

/// Whether `url` is plain HTTP to a numbered port of this machine, which is
/// how tests stand in for a provider. An installed build has no such address.
#[cfg(test)]
fn plain(url: &str) -> Option<&'static str> {
    url.strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(port, _)| port.parse::<u16>().is_ok())
        .then_some("proto = \"=http\"\nnoproxy = \"127.0.0.1\"\n")
}

#[cfg(not(test))]
fn plain(_: &str) -> Option<&'static str> {
    None
}

/// None when curl could not be run, the provider did not answer within
/// `wait`, or the answer was not a bounded HTTP reply. Redirects are not
/// followed, so the credential goes only to the host Milo named.
pub(super) fn get(request: &Request<'_>, wait: Duration) -> Option<Reply> {
    // Only HTTPS carries a key. The plain exception does not exist outside tests.
    let protocol = if request.url.starts_with("https://") { "proto = \"=https\"\n" } else { plain(request.url)? };
    if !quotable(request.url) || !quotable(request.bearer) {
        return None;
    }
    let mut config = String::with_capacity(512 + request.url.len() + request.bearer.len());
    config.push_str(protocol);
    config.push_str("silent\ninclude\nmax-redirs = 0\nmax-filesize = 262144\nconnect-timeout = 5\n");
    config.push_str(&format!("max-time = {}\n", wait.as_secs().max(1)));
    config.push_str(concat!("user-agent = \"milo/", env!("CARGO_PKG_VERSION"), "\"\n"));
    for header in request.headers {
        config.push_str(&format!("header = \"{header}\"\n"));
    }
    config.push_str(&format!("header = \"Authorization: Bearer {}\"\n", request.bearer));
    config.push_str(&format!("url = \"{}\"\n", request.url));

    // Allow curl its own limit, then a moment to exit, before it is killed.
    let deadline = Instant::now() + wait + Duration::from_secs(1);
    let program = executable();
    let mut command = Command::new(&program);
    // On Windows curl runs from its own folder, so nothing in the project's folder is looked up.
    #[cfg(windows)]
    if let Some(folder) = program.parent() {
        command.current_dir(folder);
    }
    let mut child = command
        // `-q` must come first: it keeps the user's curlrc from adding options.
        .args(["-q", "--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // The configuration is far smaller than a pipe's buffer, so this cannot block.
    let sent = child.stdin.take().is_some_and(|mut input| input.write_all(config.as_bytes()).is_ok());
    if !sent {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    parse(&sqlite::capture(child, MAX_REPLY, deadline)?)
}

/// Splits curl's `--include` output into the last response's status, its
/// retry hint and its body. A proxy or an interim response can come first.
fn parse(mut output: &[u8]) -> Option<Reply> {
    loop {
        let end = output.windows(4).position(|window| window == b"\r\n\r\n")?;
        let head = std::str::from_utf8(&output[..end]).ok()?;
        let rest = &output[end + 4..];
        let mut lines = head.split("\r\n");
        let status: u16 = lines.next()?.strip_prefix("HTTP/")?.split(' ').nth(1)?.parse().ok()?;
        if (status == 200 || (100..200).contains(&status)) && rest.starts_with(b"HTTP/") {
            output = rest;
            continue;
        }
        let retry_after = lines
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
            .and_then(|(_, value)| RetryAfter::parse(value.trim()));
        return Some(Reply { status, retry_after, body: rest.to_vec() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_response_supplies_the_status_body_and_retry_hint() {
        let reply = parse(
            b"HTTP/1.1 200 Connection established\r\n\r\nHTTP/2 429 \r\ncontent-type: application/json\r\n\
            Retry-After: 60\r\n\r\n{\"message\":\"slow down\"}",
        );
        assert_eq!(
            reply,
            Some(Reply {
                status: 429,
                retry_after: Some(RetryAfter::Seconds(60)),
                body: b"{\"message\":\"slow down\"}".to_vec()
            })
        );
        let dated = parse(b"HTTP/1.1 503 Unavailable\r\nRetry-After: Wed, 21 Oct 2026 07:28:00 GMT\r\n\r\n").unwrap();
        assert_eq!((dated.status, dated.retry_after, dated.body.len()), (503, Some(RetryAfter::At(1_792_567_680)), 0));
        for invalid in ["", "+60", "-60", "1.5", "yesterday"] {
            assert_eq!(RetryAfter::parse(invalid), None);
        }
        assert_eq!(RetryAfter::parse("99999999999999999999"), Some(RetryAfter::Seconds(i64::MAX)));
        for date in ["Sun, 06 Nov 1994 08:49:37 GMT", "Sunday, 06-Nov-94 08:49:37 GMT", "Sun Nov  6 08:49:37 1994"] {
            assert_eq!(super::super::time::http_date(date, 1_795_100_000), Some(784111777), "{date}");
        }
        for invalid in ["Thu, 31 Nov 2026 00:00:00 GMT", "Wed, 21 Oct 2026 07:28:00", "Wed, 21 Oct 2026 07:68:00 GMT"] {
            assert_eq!(RetryAfter::parse(invalid), None);
        }
        assert_eq!(parse(b"not http"), None);
        assert_eq!(parse(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n"), None);
    }

    #[test]
    fn a_credential_that_could_change_the_request_or_a_plain_remote_address_is_never_sent() {
        let wait = Duration::from_secs(1);
        for bearer in ["two words", "quote\"d", "back\\slash", "line\nbreak", "caf\u{e9}"] {
            let request = Request { url: "http://127.0.0.1:9/", bearer, headers: &[] };
            assert_eq!(get(&request, wait), None, "{bearer:?}");
        }
        let remote = ["http://example.com/", "http://127.0.0.1.example.com:80/", "http://127.0.0.1:80@example.com/"];
        for url in remote.into_iter().chain(["ftp://127.0.0.1:21/"]) {
            assert_eq!(get(&Request { url, bearer: "key", headers: &[] }, wait), None, "{url}");
        }
    }
}
