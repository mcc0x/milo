//! Starts bounded HTTP requests through the system's curl.
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

/// Only a distribution build may name an HTTPS receiver. Ordinary builds may
/// name a literal loopback test server.
/// Neither the inherited environment nor the checkout can redirect a request.
pub(crate) fn counts_protocol(url: &str) -> Option<&'static str> {
    let origin = url.strip_suffix("/v1/counts")?;
    if let Some(host) = origin.strip_prefix("https://") {
        return (cfg!(milo_distribution_counts)
            && !host.is_empty()
            && host.split('.').all(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
        .then_some("=https");
    }
    let port = origin.strip_prefix("http://127.0.0.1:")?;
    (port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().ok()? > 0).then_some("=http")
}

/// The counts caller supplies only the build-time URL and its three public fields.
/// Do not use the provider request, which carries account credentials and a versioned user agent.
pub(crate) fn post_anonymous_counts(
    url: &str,
    date: &str,
    new_install: bool,
    monthly_active: bool,
) -> Option<std::process::Child> {
    let protocol = counts_protocol(url)?;
    let body = format!(r#"{{"day":"{date}","new_install":{new_install},"monthly_active":{monthly_active}}}"#);
    #[cfg(unix)]
    let command = {
        let mut command = Command::new(executable());
        command.env_clear().current_dir("/");
        command
    };
    #[cfg(windows)]
    let command = counts_curl()?;
    send_counts(command, url, protocol, &body)
}

#[cfg(windows)]
fn counts_curl() -> Option<Command> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;
    use std::os::windows::process::CommandExt as _;
    use std::path::PathBuf;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut buffer = [0_u16; 260];
    // SAFETY: buffer is writable for exactly the advertised number of UTF-16 code units.
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), 260) };
    let len = usize::try_from(len).ok().filter(|&len| len > 0 && len < buffer.len())?;
    let system = PathBuf::from(OsString::from_wide(&buffer[..len]));
    if !system.is_absolute() {
        return None;
    }
    let mut command = Command::new(system.join("curl.exe"));
    // The system directory also supplies the working directory for DLL searches.
    // Do not inherit SystemRoot, proxy settings, or certificate overrides.
    command.env_clear().env("SystemRoot", system.parent()?).current_dir(&system).creation_flags(CREATE_NO_WINDOW);
    Some(command)
}

fn send_counts(mut command: Command, url: &str, protocol: &str, body: &str) -> Option<std::process::Child> {
    command.arg("-q");
    #[cfg(windows)]
    command.arg("--no-ssl-auto-client-cert");
    crate::fs_ops::spawn(
        command
            .args([
                "--proto",
                protocol,
                "--noproxy",
                "*",
                "--globoff",
                "--http1.1",
                "--retry",
                "0",
                "--max-time",
                "3",
                "--max-filesize",
                "1024",
                "--user-agent",
                "",
                "--header",
                "Content-Type: application/json",
                "--data-binary",
                body,
                "--url",
                url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .ok()
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

    mod telemetry {
        use super::*;

        #[test]
        fn a_missing_or_invalid_executable_drops_the_attempt() {
            let f = crate::fixture::Fixture::folder();
            let path = f.root.join("curl.exe");
            let local = "http://127.0.0.1:1/v1/counts";
            assert!(send_counts(Command::new(&path), local, "=http", "{}").is_none());
            f.write("curl.exe", "Not an executable.");
            assert!(send_counts(Command::new(&path), local, "=http", "{}").is_none());
        }

        #[cfg(windows)]
        #[test]
        fn a_checkout_cannot_substitute_its_own_curl_or_system_root() {
            use std::path::Path;
            const CHILD: &str = "MILO_TEST_COUNTS_FAKE_ROOT";
            if let Some(fake) = std::env::var_os(CHILD) {
                let command = counts_curl().unwrap();
                let system = command.get_current_dir().unwrap();
                let program = Path::new(command.get_program());
                assert!(program.is_absolute() && !program.starts_with(&fake));
                assert_eq!(program, system.join("curl.exe"));
                assert_eq!(command.get_envs().count(), 1);
                let (name, root) = command.get_envs().next().unwrap();
                assert_eq!(name, "SystemRoot");
                assert_eq!(root.map(Path::new), system.parent());
                return;
            }
            let f = crate::fixture::Fixture::folder();
            f.write("System32/curl.exe", "Not a system binary.");
            f.write("curl.exe", "Not a system binary.");
            let test = "usage::http::tests::telemetry::a_checkout_cannot_substitute_its_own_curl_or_system_root";
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test])
                .current_dir(&f.root)
                .env(CHILD, &f.root)
                .env("PATH", &f.root)
                .env("SystemRoot", &f.root)
                .env("WINDIR", &f.root)
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
    }

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
