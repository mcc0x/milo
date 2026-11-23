//! An explicit local URL, the user's placement choice, and a Rex-owned terminal anchor.

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

pub(crate) const EXPECTED: &str = "expected milo preview URL [--placement right|left|above|below]";
pub(crate) const HELP: &str = "\
Usage: milo preview URL [--placement right|left|above|below]

Open a persistent local browser pane beside the working terminal in this Rex tab.
Right is the default. Change Preview placement in Settings or override it once.
Use HTTP or HTTPS on localhost, 127.0.0.1 or [::1], without credentials, query or fragment.

Keep working in the sibling terminal or drag the Rex divider. The page stays
loaded. Command-Shift-Return returns keyboard focus; Command-W in the preview
closes it. Select an element, review its structural path, then choose Copy.
Nothing is sent to an agent.

Experimental macOS overlay: placement is approximate. Start in a Mac-created
terminal with one visible local Rex window. No Accessibility, Screen Recording,
Remote Control, port scanning or default-browser change. Remote and floating
previews are not supported.

Rex can report a tab change but not every session or host switch inside the same
Mac window. The preview can remain over that other session indefinitely. Close
the preview before switching sessions or hosts in the pinned window.";

/// Restrict the authority before passing the literal URL through Rex. WebKit checks
/// the parsed origin again on navigation. Never echo a rejected URL in an error.
pub(crate) fn url(text: &str) -> Result<&str, &'static str> {
    const INVALID: &str =
        "use an HTTP(S) URL on localhost, 127.0.0.1 or [::1], without credentials, a query or a fragment";
    if text.len() > 4096 {
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
    let port = ["localhost", "127.0.0.1", "[::1]"]
        .into_iter()
        .find_map(|host| {
            authority
                .get(..host.len())
                .filter(|prefix| prefix.eq_ignore_ascii_case(host))
                .map(|_| &authority[host.len()..])
        })
        .ok_or(INVALID)?;
    if !port.is_empty()
        && !port
            .strip_prefix(':')
            .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u16>().is_ok_and(|p| p != 0))
    {
        return Err(INVALID);
    }
    Ok(text)
}

#[cfg(target_os = "macos")]
pub(crate) fn open(url: &str, placement: Option<PreviewPlacement>) -> Result<(), String> {
    crate::rex::open_preview(url, placement)
}

#[cfg(target_os = "macos")]
pub(crate) use mac::{anchor, pin_window};

#[cfg(not(target_os = "macos"))]
pub(crate) fn open(_: &str, _: Option<PreviewPlacement>) -> Result<(), String> {
    Err("WebKit preview requires a local Rex session on macOS; no browser was opened".into())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn anchor(url: &str, _: Anchor<'_>) -> Result<(), String> {
    open(url, None)
}

#[cfg(test)]
mod tests {
    use super::url;

    #[test]
    fn explicit_loopback_authorities_only() {
        for good in ["http://localhost:3000", "HTTPS://LOCALHOST/dev", "http://127.0.0.1", "http://[::1]:8080/a%2Fb"] {
            assert_eq!(url(good), Ok(good));
        }
        for bad in [
            "http://localhost.example",
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
