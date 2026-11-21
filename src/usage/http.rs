//! Asks a provider one bounded question through the system's curl.
//!
//! The credential travels on curl's standard input, never in its arguments or
//! environment, so other processes cannot read it from the process table.

use std::ffi::OsStr;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::sqlite;

/// A reply larger than this is not one of the small reports Milo asks for.
const MAX_REPLY: usize = 256 * 1024;

pub(super) fn executable() -> &'static OsStr {
    OsStr::new("/usr/bin/curl")
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
    /// Seconds the provider asked callers to wait, when it said so in seconds.
    pub(super) retry_after: Option<u64>,
    pub(super) body: Vec<u8>,
}

/// Whether `text` can sit inside a quoted value of curl's configuration
/// without ending the value or starting an escape.
pub(super) fn quotable(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\')
}

/// Whether `url` is plain HTTP to a numbered port of this machine, which is
/// how tests stand in for a provider.
fn loopback(url: &str) -> bool {
    url.strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(port, _)| port.parse::<u16>().is_ok())
}

/// None when curl could not be run, the provider did not answer within
/// `wait`, or the answer was not a bounded HTTP reply. Redirects are not
/// followed, so the credential goes only to the host Milo named.
pub(super) fn get(request: &Request<'_>, wait: Duration) -> Option<Reply> {
    let secure = request.url.starts_with("https://");
    if !secure && !loopback(request.url) {
        return None;
    }
    if !quotable(request.url) || !quotable(request.bearer) {
        return None;
    }
    let mut config = String::with_capacity(512 + request.url.len() + request.bearer.len());
    config.push_str(if secure { "proto = \"=https\"\n" } else { "proto = \"=http\"\nnoproxy = \"127.0.0.1\"\n" });
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
    let mut child = Command::new(executable())
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
        if rest.starts_with(b"HTTP/") {
            output = rest;
            continue;
        }
        let retry_after = lines
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
            .and_then(|(_, value)| value.trim().parse().ok());
        return Some(Reply { status, retry_after, body: rest.to_vec() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_response_is_the_reply_and_its_retry_hint_is_read_in_seconds_only() {
        let reply = parse(
            b"HTTP/1.1 200 Connection established\r\n\r\nHTTP/2 429 \r\ncontent-type: application/json\r\n\
            Retry-After: 60\r\n\r\n{\"message\":\"slow down\"}",
        );
        assert_eq!(
            reply,
            Some(Reply { status: 429, retry_after: Some(60), body: b"{\"message\":\"slow down\"}".to_vec() })
        );
        let dated = parse(b"HTTP/1.1 503 Unavailable\r\nRetry-After: Wed, 21 Oct 2026 07:28:00 GMT\r\n\r\n").unwrap();
        assert_eq!((dated.status, dated.retry_after, dated.body.len()), (503, None, 0));
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
