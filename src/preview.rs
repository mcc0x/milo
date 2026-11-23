//! An explicit browser address, the user's placement choice, and a Rex-owned terminal anchor.

use crate::preferences::PreviewPlacement;

#[cfg(target_os = "macos")]
mod mac;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Anchor<'a> {
    pub main: &'a str,
    pub window: &'a str,
    pub owner: &'a str,
    pub owner_pid: u32,
    pub native_window_id: std::num::NonZeroU32,
}

pub(crate) const URL_MAX: usize = 4096;
pub(crate) const EXPECTED: &str = "expected milo preview URL [--placement right|left|above|below]";
pub(crate) const HELP: &str = "\
Usage: milo preview URL [--placement right|left|above|below]

Open a browser pane beside the working terminal in this local Rex tab.
For your dev server, try milo preview http://localhost:3000. HTTP(S) sites also work.
Credentials in URLs are not accepted. Enter addresses with a query or fragment in the pane.
Right is the default. Change Preview placement in Settings or override it once.

Keep working in the sibling terminal or drag the Rex divider. The page stays
loaded. Command-L edits the address; Return opens it. Command-Shift-Return
returns keyboard focus; Command-W in the preview closes it. Only the original
local dev origin offers Select → review → Copy. Nothing is sent to an agent.
A link leaving the dev origin shows its destination. Choose Open link or Stay.

Experimental macOS overlay: placement is approximate. Start in a Mac-created
terminal with one visible local Rex window. No Accessibility, Screen Recording,
Remote Control, port scanning or default-browser change. Remote and floating
previews are not supported.

Rex can report a tab change but not every session or host switch inside the same
Mac window. The preview can remain over that other session indefinitely. Close
the preview before switching sessions or hosts in the pinned window.";

// Use unambiguous ASCII hosts in command arguments. In particular, don't let
// WebKit canonicalize legacy numeric spellings into a picker-eligible loopback host.
fn valid_authority(authority: &str) -> bool {
    let port = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((host, rest)) = bracketed.split_once(']') else { return false };
        let Ok(ip) = host.parse::<std::net::Ipv6Addr>() else { return false };
        if ip.is_loopback() && host != "::1" {
            return false;
        }
        rest
    } else {
        let (host, rest) = authority.split_at(authority.find(':').unwrap_or(authority.len()));
        let domain = host.strip_suffix('.').unwrap_or(host);
        if domain.is_empty()
            || domain.len() > 253
            || !domain.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return false;
        }
        let last = domain.rsplit('.').next().unwrap_or_default();
        let numeric = last.bytes().all(|b| b.is_ascii_digit())
            || last
                .strip_prefix("0x")
                .or_else(|| last.strip_prefix("0X"))
                .is_some_and(|s| s.bytes().all(|b| b.is_ascii_hexdigit()));
        if numeric && host.parse::<std::net::Ipv4Addr>().is_err() {
            return false;
        }
        rest
    };
    port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u16>().is_ok_and(|p| p != 0))
}

/// The launch command may appear in shell history. Keep query strings and
/// fragments in the native address field; never echo a rejected argument.
pub(crate) fn url(text: &str) -> Result<&str, &'static str> {
    const INVALID: &str =
        "use an HTTP(S) URL with a host and no credentials; enter addresses with a query or fragment in the pane";
    if text.len() > URL_MAX {
        return Err(INVALID);
    }
    let (scheme, rest) = text.split_once("://").ok_or(INVALID)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(INVALID);
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    // Require an already encoded RFC 3986 path. Do not let NSURL rewrite the
    // requested resource differently on different SDK versions.
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            if !(bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
                && bytes.next().is_some_and(|b| b.is_ascii_hexdigit()))
            {
                return Err("use a percent-encoded URL path with valid %HH escapes");
            }
        } else if !(byte.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&byte)) {
            return Err(INVALID);
        }
    }
    if !valid_authority(authority) {
        return Err(INVALID);
    }
    Ok(text)
}

#[cfg(target_os = "macos")]
pub(crate) fn open(url: &str, placement: Option<PreviewPlacement>) -> Result<(), String> {
    crate::rex::open_preview(url, placement)
}

#[cfg(target_os = "macos")]
pub(crate) use mac::{StartupSocket, anchor, pin_window};

#[cfg(not(target_os = "macos"))]
pub(crate) fn open(_: &str, _: Option<PreviewPlacement>) -> Result<(), String> {
    Err("WebKit preview requires a local Rex session on macOS; no browser was opened".into())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn anchor(_: &std::path::Path, _: Anchor<'_>) -> Result<(), String> {
    Err("WebKit preview requires a local Rex session on macOS".into())
}

#[cfg(test)]
mod tests {
    use super::url;

    #[test]
    fn launch_accepts_http_sites_without_confusing_them_with_loopback() {
        for good in [
            "http://localhost:3000",
            "HTTPS://LOCALHOST/dev",
            "http://127.0.0.1",
            "http://[::1]:8080/a%2Fb",
            "https://localhost.example",
            "https://www.example.org/index.html",
            "http://192.0.2.1:8080",
            "http://[2001:db8::1]/",
            "https://xn--bcher-kva.example/",
            "https://example.org./",
        ] {
            assert_eq!(url(good), Ok(good));
        }
        for bad in [
            "https://",
            "https:///etc",
            "https://-bad.example",
            "http://a..b",
            "http://example:443:80",
            "http://0x7f000001/",
            "http://2130706433/",
            "http://0177.0.0.1/",
            "http://[0:0:0:0:0:0:0:1]/",
            "http://localhost:80@evil.net",
            "http://127.1",
            "http://localhost:0",
            "http://[::1]:65536",
            "file://localhost/etc",
            "http://user@localhost/",
            "http://localhost\\@external",
            "http://localhost/#secret",
            "http://localhost/?token=value",
            "http://localhost/a\nb",
            "http://localhost/a%2",
            "http://localhost/raw[part]",
            "http://localhost/café",
        ] {
            assert!(url(bad).is_err());
        }
    }
}
