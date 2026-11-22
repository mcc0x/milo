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
}

pub(crate) const EXPECTED: &str = "expected milo preview URL [--placement right|left|above|below]";
pub(crate) const HELP: &str = "Usage: milo preview URL [--placement right|left|above|below]\n\nOpen an already-running local dev site in this Rex tab on macOS. Example: milo preview http://localhost:3000. Milo adds a tiled terminal anchor beside the main working terminal, not the sidebar. The default is right; change Preview placement in Settings or use --placement once.\n\nUse an HTTP or HTTPS URL on localhost, 127.0.0.1 or [::1], without credentials, a query or a fragment. Milo does not look for servers. The WebKit overlay requires Accessibility permission to locate its pane and hides when its bounds are uncertain. A remote session is not supported.\n\nPick an element to review its structural location, then choose Copy to put it on the clipboard. Nothing is sent to an agent. With the page focused, press Command-Shift-Return to return to the working terminal or Command-W to close the preview. You can also close the anchor pane in Rex, even when the page is hidden.";

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
pub(crate) use mac::{anchor, require_access};

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
