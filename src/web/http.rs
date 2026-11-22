use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::{self, IoSlice, Read, Write};
use std::sync::Arc;

use super::assets::Asset;

const MAX_HEAD: usize = 8 * 1024;
const MAX_BODY: usize = 16 * 1024;
const READ_CHUNK: usize = 2048;
const SECURITY_HEADERS: &str = "Cache-Control: no-store\r\n\
    Referrer-Policy: no-referrer\r\n\
    X-Content-Type-Options: nosniff\r\n\
    Cross-Origin-Opener-Policy: same-origin\r\n\
    Cross-Origin-Resource-Policy: same-origin\r\n\
    Connection: close\r\n";
const POLICY_START: &str = "Content-Security-Policy: default-src 'none'; script-src 'self'";
const POLICY_END: &str = "; style-src 'self'; img-src 'self'; font-src 'self'; connect-src 'self'; \
    base-uri 'none'; frame-ancestors 'none'; form-action 'none'\r\n";
const JSON: &str = "application/json; charset=utf-8";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Route {
    Static(Asset),
    State,
    MenuIndicator,
    Action,
    Close,
    TelegramChats,
    HostedConnect,
    Transfer,
}

impl Route {
    pub(super) fn authenticated(&self) -> bool {
        !matches!(self, Self::Static(_))
    }
}

pub(super) struct Access<'a> {
    pub(super) host: &'a str,
    pub(super) origin: &'a str,
    pub(super) bearer: &'a str,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Rejection {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    LengthRequired,
    BodyTooLarge,
    UnsupportedMediaType,
    HeadTooLarge,
}

impl Rejection {
    pub(super) fn response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest => ("400 Bad Request", "Invalid HTTP request"),
            Self::Unauthorized => ("401 Unauthorized", "Missing or invalid session token"),
            Self::Forbidden => ("403 Forbidden", "Unexpected host or origin"),
            Self::NotFound => ("404 Not Found", "Unknown endpoint"),
            Self::LengthRequired => ("411 Length Required", "Missing Content-Length"),
            Self::BodyTooLarge => ("413 Content Too Large", "Request body is too large"),
            Self::UnsupportedMediaType => ("415 Unsupported Media Type", "Expected application/json"),
            Self::HeadTooLarge => ("431 Request Header Fields Too Large", "Request headers are too large"),
        };
        Response::error(status, message)
    }
}

// This server accepts one HTTP/1.1 request per socket, with only Content-Length framing.
// `head` ends before the CRLF pair separating the headers from the body.
fn request(head: &[u8], access: &Access<'_>) -> Result<(Route, usize), Rejection> {
    let head = std::str::from_utf8(head).map_err(|_| Rejection::BadRequest)?;
    let mut lines = head.split("\r\n");
    let mut start = lines.next().ok_or(Rejection::BadRequest)?.split(' ');
    let (Some(method), Some(path), Some("HTTP/1.1"), None) = (start.next(), start.next(), start.next(), start.next())
    else {
        return Err(Rejection::BadRequest);
    };
    let mut host = None;
    let mut origin = None;
    let mut authorization = None;
    let mut length = None;
    let mut content_type = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Rejection::BadRequest)?;
        if name.is_empty() || !name.bytes().all(header_name_byte) || value.bytes().any(invalid_header_value_byte) {
            return Err(Rejection::BadRequest);
        }
        let value = value.trim_matches([' ', '\t']);
        let slot = if name.eq_ignore_ascii_case("host") {
            &mut host
        } else if name.eq_ignore_ascii_case("origin") {
            &mut origin
        } else if name.eq_ignore_ascii_case("authorization") {
            &mut authorization
        } else if name.eq_ignore_ascii_case("content-length") {
            &mut length
        } else if name.eq_ignore_ascii_case("content-type") {
            &mut content_type
        } else if name.eq_ignore_ascii_case("transfer-encoding") || name.eq_ignore_ascii_case("expect") {
            return Err(Rejection::BadRequest);
        } else {
            continue;
        };
        if slot.replace(value).is_some() {
            return Err(Rejection::BadRequest);
        }
    }

    if host != Some(access.host)
        || origin.is_some_and(|origin| origin != access.origin)
        || (method == "POST" && origin.is_none())
    {
        return Err(Rejection::Forbidden);
    }
    let route = match (method, path) {
        ("GET", "/api/state") => Route::State,
        ("GET", "/api/menu-indicator") => Route::MenuIndicator,
        ("POST", "/api/action") => Route::Action,
        ("POST", "/api/close") => Route::Close,
        ("POST", "/api/telegram-chats") => Route::TelegramChats,
        ("POST", "/api/hosted-connect") => Route::HostedConnect,
        ("POST", "/api/transfer") => Route::Transfer,
        ("GET", path) => Route::Static(Asset::at(path).ok_or(Rejection::NotFound)?),
        _ => return Err(Rejection::NotFound),
    };
    if route.authenticated() && !authorization.is_some_and(|header| same_secret(header, access.bearer)) {
        return Err(Rejection::Unauthorized);
    }
    let expects_json =
        matches!(route, Route::Action | Route::Close | Route::TelegramChats | Route::HostedConnect | Route::Transfer);
    let limit = match route {
        Route::HostedConnect => crate::alerts::relay::REQUEST_LIMIT,
        // A bundle is text in a JSON string, where a line end takes two bytes.
        Route::Transfer => 2 * crate::setup::transfer::BUNDLE_LIMIT + 1024,
        _ => MAX_BODY,
    };
    let body_len = match length {
        Some(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
            value.parse::<usize>().ok().filter(|&len| len <= limit).ok_or(Rejection::BodyTooLarge)?
        }
        Some(_) => return Err(Rejection::BadRequest),
        None if expects_json => return Err(Rejection::LengthRequired),
        None => 0,
    };
    if expects_json {
        if !content_type.is_some_and(|value| value.eq_ignore_ascii_case("application/json")) {
            return Err(Rejection::UnsupportedMediaType);
        }
    } else if body_len != 0 {
        return Err(Rejection::BadRequest);
    }
    Ok((route, body_len))
}

pub(super) fn close_version(body: &[u8]) -> Option<u64> {
    let mut version = None;
    let mut valid = true;
    crate::json::settings_object(std::str::from_utf8(body).ok()?, |member| {
        if member.key.is("version") {
            valid &= version.replace(member.value).is_none();
        } else {
            valid = false;
        }
    })?;
    if !valid {
        return None;
    }
    version?.unsigned()
}

pub(super) fn telegram_token(body: &[u8]) -> Option<Cow<'_, str>> {
    let mut token = None;
    let mut valid = true;
    crate::json::settings_object(std::str::from_utf8(body).ok()?, |member| {
        valid &= member.key.is("token") && token.replace(member.value).is_none();
    })?;
    token.filter(|_| valid)?.string(128)
}

#[derive(Default)]
pub(super) struct Request {
    bytes: Vec<u8>,
    pending: Option<PendingBody>,
}

struct PendingBody {
    route: Route,
    start: usize,
    end: usize,
}

pub(super) enum Input {
    Wait,
    Disconnected,
    Rejected(Rejection),
    Ready(Route),
}

impl Request {
    pub(super) fn read_from(&mut self, socket: &mut impl Read, access: &Access<'_>) -> io::Result<Input> {
        let limit = self.pending.as_ref().map_or(MAX_HEAD, |body| body.end);
        let previous = self.bytes.len();
        self.bytes.resize(limit.min(previous + READ_CHUNK), 0);
        let read = socket.read(&mut self.bytes[previous..]);
        self.bytes.truncate(previous + read.as_ref().copied().unwrap_or(0));
        match read {
            Ok(0) => return Ok(Input::Disconnected),
            Ok(_) => {}
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                return Ok(Input::Wait);
            }
            Err(error) => return Err(error),
        }

        if self.pending.is_none() {
            let scan_from = previous.saturating_sub(3);
            let header_end = self.bytes[scan_from..].windows(4).position(|window| window == b"\r\n\r\n");
            let Some(header_end) = header_end.map(|offset| offset + scan_from) else {
                return Ok(if self.bytes.len() == MAX_HEAD {
                    Input::Rejected(Rejection::HeadTooLarge)
                } else {
                    Input::Wait
                });
            };
            let (route, length) = match request(&self.bytes[..header_end], access) {
                Ok(parsed) => parsed,
                Err(rejection) => return Ok(Input::Rejected(rejection)),
            };
            let start = header_end + 4;
            self.pending = Some(PendingBody { route, start, end: start + length });
        }
        let body = self.pending.as_ref().expect("header parsed");
        if self.bytes.len() < body.end {
            return Ok(Input::Wait);
        }
        if self.bytes.len() > body.end {
            return Ok(Input::Rejected(Rejection::BadRequest));
        }
        Ok(Input::Ready(body.route))
    }

    pub(super) fn body(&self) -> &[u8] {
        let body = self.pending.as_ref().expect("request is ready");
        &self.bytes[body.start..body.end]
    }
}

fn header_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn invalid_header_value_byte(byte: u8) -> bool {
    (byte < b' ' && byte != b'\t') || byte == 0x7f
}

fn same_secret(header: &str, expected: &str) -> bool {
    let header = header.as_bytes();
    let expected = expected.as_bytes();
    header.len() == expected.len()
        && header.iter().zip(expected).fold(0, |difference, (a, b)| difference | (a ^ b)) == 0
}

pub(super) struct Response {
    header: String,
    body: Body,
    sent: usize,
}

enum Body {
    Borrowed(&'static [u8]),
    Owned(Vec<u8>),
    Snapshot(Arc<String>),
}

impl AsRef<[u8]> for Body {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(bytes) => bytes,
            Self::Snapshot(state) => state.as_bytes(),
        }
    }
}

impl Response {
    fn new(status: &str, content_type: Option<&str>, body: Body, scripts: &str) -> Self {
        let capacity = SECURITY_HEADERS.len() + POLICY_START.len() + scripts.len() + POLICY_END.len() + 128;
        let mut header = String::with_capacity(capacity);
        let _ = write!(header, "HTTP/1.1 {status}\r\n{SECURITY_HEADERS}{POLICY_START}{scripts}{POLICY_END}");
        if status == "401 Unauthorized" {
            header.push_str("WWW-Authenticate: Bearer\r\n");
        }
        if let Some(content_type) = content_type {
            let _ = write!(header, "Content-Type: {content_type}\r\nContent-Length: {}\r\n", body.as_ref().len());
        }
        header.push_str("\r\n");
        Self { header, body, sent: 0 }
    }

    pub(super) fn asset(content_type: &str, body: &'static [u8], scripts: &str) -> Self {
        Self::new("200 OK", Some(content_type), Body::Borrowed(body), scripts)
    }

    pub(super) fn json(body: Arc<String>) -> Self {
        Self::new("200 OK", Some(JSON), Body::Snapshot(body), "")
    }

    pub(super) fn json_owned(body: String) -> Self {
        Self::new("200 OK", Some(JSON), Body::Owned(body.into_bytes()), "")
    }

    pub(super) fn pairing_pending() -> Self {
        Self::new("202 Accepted", Some(JSON), Body::Borrowed(br#"{"pending":true}"#), "")
    }

    pub(super) fn pairing_discarded(cleanup_confirmed: bool) -> Self {
        let body: &[u8] =
            if cleanup_confirmed { br#"{"cleanupConfirmed":true}"# } else { br#"{"cleanupConfirmed":false}"# };
        Self::new("410 Gone", Some(JSON), Body::Borrowed(body), "")
    }

    pub(super) fn no_content() -> Self {
        Self::new("204 No Content", None, Body::Borrowed(b""), "")
    }

    pub(super) fn bad_request(message: &str) -> Self {
        Self::error("400 Bad Request", message)
    }

    pub(super) fn conflict(message: &str) -> Self {
        Self::error("409 Conflict", message)
    }

    pub(super) fn bad_gateway(message: &str) -> Self {
        Self::error("502 Bad Gateway", message)
    }

    pub(super) fn busy() -> Self {
        Self::error(
            "503 Service Unavailable",
            "The editor is busy. Wait for the other action to finish, then try again.",
        )
    }

    pub(super) fn error(status: &str, message: &str) -> Self {
        let mut body = String::with_capacity(message.len() + 12);
        body.push_str("{\"error\":\"");
        for character in message.chars() {
            match character {
                '"' => body.push_str("\\\""),
                '\\' => body.push_str("\\\\"),
                '\u{0}'..='\u{1f}' => {
                    let _ = write!(body, "\\u{:04x}", u32::from(character));
                }
                _ => body.push(character),
            }
        }
        body.push_str("\"}");
        Self::new(status, Some(JSON), Body::Owned(body.into_bytes()), "")
    }

    // Keep the fixed assets borrowed; the first write can send headers and body together.
    pub(super) fn write_to(&mut self, socket: &mut impl Write) -> io::Result<bool> {
        let header = self.header.as_bytes();
        let body = self.body.as_ref();
        let written = if self.sent < header.len() {
            socket.write_vectored(&[IoSlice::new(&header[self.sent..]), IoSlice::new(body)])
        } else {
            socket.write(&body[self.sent - header.len()..])
        };
        match written {
            Ok(0) => Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => {
                self.sent += count;
                Ok(self.sent == header.len() + body.len())
            }
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCESS: Access<'_> =
        Access { host: "127.0.0.1:4321", origin: "http://127.0.0.1:4321", bearer: "Bearer the-token" };

    fn incoming(method: &str, path: &str, headers: &str) -> Result<(Route, usize), Rejection> {
        let input = format!("{method} {path} HTTP/1.1\r\nHost: {}{headers}", ACCESS.host);
        request(input.as_bytes(), &ACCESS)
    }

    #[test]
    fn exposed_routes_are_exact_and_state_requires_bearer() {
        for path in ["/", "/favicon.png", "/milo.webp", "/rex.png"] {
            assert!(matches!(incoming("GET", path, ""), Ok((Route::Static(_), 0))));
        }
        for path in [
            "/index.html",
            "/index.txt",
            "/dev/version",
            "/app.js",
            "/favicon.png?other",
            "/_next/static/chunks/unknown.js",
            "/_next/static/chunks/../index.html",
        ] {
            assert_eq!(incoming("GET", path, ""), Err(Rejection::NotFound), "{path}");
        }
        assert_eq!(incoming("GET", "/api/state", ""), Err(Rejection::Unauthorized));
        assert_eq!(incoming("GET", "/api/state", "\r\nAuthorization: Bearer bad-token"), Err(Rejection::Unauthorized));
        let bearer = "\r\nAuthorization: Bearer the-token";
        assert_eq!(incoming("GET", "/api/state", bearer), Ok((Route::State, 0)));
        assert_eq!(incoming("GET", "/api/state?token=the-token", bearer), Err(Rejection::NotFound));
        assert_eq!(incoming("GET", "http://127.0.0.1:4321/api/state", bearer), Err(Rejection::NotFound));
        assert_eq!(incoming("GET", "/%61pi/state", bearer), Err(Rejection::NotFound));
        assert_eq!(incoming("GET", "/../../etc/passwd", bearer), Err(Rejection::NotFound));
        assert_eq!(request(b"GET / HTTP/1.1\r\nHost: rebound.example:4321", &ACCESS), Err(Rejection::Forbidden));
    }

    #[test]
    fn posts_require_the_exact_origin_and_token() {
        let bearer = "\r\nAuthorization: Bearer the-token";
        let valid = "\r\nAuthorization: Bearer the-token\r\nOrigin: http://127.0.0.1:4321";
        for origin in ["http://localhost:4321", "http://127.0.0.1:4322", "null", "http://127.0.0.1:4321/"] {
            let headers = format!("{bearer}\r\nOrigin: {origin}");
            assert_eq!(incoming("POST", "/api/close", &headers), Err(Rejection::Forbidden));
            assert_eq!(incoming("GET", "/api/state", &headers), Err(Rejection::Forbidden));
        }
        assert_eq!(incoming("POST", "/api/close", bearer), Err(Rejection::Forbidden));
        assert_eq!(incoming("POST", "/api/close", "\r\nOrigin: http://127.0.0.1:4321"), Err(Rejection::Unauthorized));
        let headers = format!("{valid}\r\nContent-Type: application/json\r\nContent-Length: 13");
        assert_eq!(incoming("POST", "/api/close", &headers), Ok((Route::Close, 13)));
    }

    #[test]
    fn telegram_chats_requires_an_authenticated_same_origin_json_post() {
        let path = "/api/telegram-chats";
        let bearer = "\r\nAuthorization: Bearer the-token";
        let origin = "\r\nOrigin: http://127.0.0.1:4321";
        let headers = format!("{bearer}{origin}\r\nContent-Type: application/json\r\nContent-Length: 2");
        assert_eq!(incoming("POST", path, &headers), Ok((Route::TelegramChats, 2)));
        assert_eq!(incoming("GET", path, bearer), Err(Rejection::NotFound));
        assert_eq!(incoming("POST", path, bearer), Err(Rejection::Forbidden));
        assert_eq!(incoming("POST", path, origin), Err(Rejection::Unauthorized));
        assert_eq!(incoming("POST", path, &format!("{bearer}{origin}")), Err(Rejection::LengthRequired));
        assert_eq!(
            incoming("POST", path, &format!("{bearer}{origin}\r\nContent-Length: 2")),
            Err(Rejection::UnsupportedMediaType)
        );
    }

    #[test]
    fn hosted_connect_is_an_exact_authenticated_same_origin_json_post_with_a_smaller_limit() {
        let path = "/api/hosted-connect";
        let bearer = "\r\nAuthorization: Bearer the-token";
        let origin = "\r\nOrigin: http://127.0.0.1:4321";
        let write = "\r\nContent-Type: application/json\r\nContent-Length: 1024";
        let valid = format!("{bearer}{origin}{write}");
        assert_eq!(incoming("POST", path, &valid), Ok((Route::HostedConnect, 1024)));
        assert_eq!(incoming("GET", path, &valid), Err(Rejection::NotFound));
        assert_eq!(incoming("POST", "/api/hosted-connect?target=other", &valid), Err(Rejection::NotFound));
        assert_eq!(incoming("POST", path, &format!("{bearer}{write}")), Err(Rejection::Forbidden));
        assert_eq!(incoming("POST", path, &format!("{origin}{write}")), Err(Rejection::Unauthorized));
        assert_eq!(incoming("POST", path, &valid.replace("1024", "1025")), Err(Rejection::BodyTooLarge));
        assert_eq!(
            incoming("POST", path, &valid.replace("application/json", "text/plain")),
            Err(Rejection::UnsupportedMediaType)
        );
    }

    #[test]
    fn telegram_discovery_accepts_only_a_single_bounded_token_string() {
        assert_eq!(telegram_token(br#"{"token":"key"}"#).as_deref(), Some("key"));
        for body in [
            "{}",
            r#"{"token":1}"#,
            r#"{"token":"a","token":"b"}"#,
            r#"{"token":"a","offset":1}"#,
            r#"{"token":"a"}null"#,
        ] {
            assert_eq!(telegram_token(body.as_bytes()), None);
        }
        assert_eq!(telegram_token(format!(r#"{{"token":"{}"}}"#, "x".repeat(129)).as_bytes()), None);
    }

    #[test]
    fn rejects_ambiguous_headers_and_body_framing() {
        let valid = "\r\nAuthorization: Bearer the-token\r\nOrigin: http://127.0.0.1:4321";
        let json = "\r\nContent-Type: application/json";
        for extra in [
            "\r\nHOST: 127.0.0.1:4321",
            "\r\norigin: http://127.0.0.1:4321",
            "\r\nauthorization: Bearer the-token",
            "\r\n Host: 127.0.0.1:4321",
            "\r\nHost : 127.0.0.1:4321",
            "\r\nTransfer-Encoding: chunked",
            "\r\nContent-Length: 0\r\ncontent-length: 0",
            "\r\nContent-Length: 0, 0",
        ] {
            assert_eq!(incoming("POST", "/api/close", &format!("{valid}{extra}")), Err(Rejection::BadRequest));
        }
        assert_eq!(incoming("POST", "/api/action", valid), Err(Rejection::LengthRequired));
        assert_eq!(
            incoming("POST", "/api/action", &format!("{valid}\r\nContent-Length: 2")),
            Err(Rejection::UnsupportedMediaType)
        );
        assert_eq!(
            incoming("POST", "/api/action", &format!("{valid}{json}\r\nContent-Length: {}", MAX_BODY + 1)),
            Err(Rejection::BodyTooLarge)
        );
        assert_eq!(
            incoming("POST", "/api/action", &format!("{valid}{json}\r\nContent-Length: 2")),
            Ok((Route::Action, 2))
        );
        assert_eq!(incoming("POST", "/api/close", valid), Err(Rejection::LengthRequired));
        assert_eq!(
            incoming("POST", "/api/close", &format!("{valid}\r\nContent-Length: 13")),
            Err(Rejection::UnsupportedMediaType)
        );
    }

    #[test]
    fn close_requires_a_single_unsigned_version() {
        assert_eq!(close_version(br#"{"version":7}"#), Some(7));
        for body in [
            "",
            "{}",
            r#"{"version":"7"}"#,
            r#"{"version":7.0}"#,
            r#"{"version":-1}"#,
            r#"{"version":18446744073709551616}"#,
            r#"{"version":7,"version":8}"#,
            r#"{"version":7,"action":"close"}"#,
            r#"{"version":7}null"#,
        ] {
            assert_eq!(close_version(body.as_bytes()), None, "{body}");
        }
    }

    #[test]
    fn waits_for_the_entire_body_across_reads() {
        let mut request = Request::default();
        let mut head = &b"POST /api/action HTTP/1.1\r\nHost: 127.0.0.1:4321\r\nOrigin: http://127.0.0.1:4321\r\n\
            Authorization: Bearer the-token\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r"[..];
        assert!(matches!(request.read_from(&mut head, &ACCESS).unwrap(), Input::Wait));
        assert!(matches!(request.read_from(&mut &b"\n{"[..], &ACCESS).unwrap(), Input::Wait));
        let final_read = request.read_from(&mut &b"}"[..], &ACCESS).unwrap();
        assert!(matches!(final_read, Input::Ready(Route::Action)));
        assert_eq!(request.body(), b"{}");
    }

    #[test]
    fn rejects_overlong_headers_and_excess_bytes_already_received() {
        let mut request = Request::default();
        let bytes = [b'x'; MAX_HEAD];
        let mut head = &bytes[..];
        for _ in 1..MAX_HEAD / READ_CHUNK {
            assert!(matches!(request.read_from(&mut head, &ACCESS).unwrap(), Input::Wait));
        }
        assert!(matches!(request.read_from(&mut head, &ACCESS).unwrap(), Input::Rejected(Rejection::HeadTooLarge)));

        let mut request = Request::default();
        let mut pipelined = &b"GET / HTTP/1.1\r\nHost: 127.0.0.1:4321\r\n\r\nGET / HTTP/1.1\r\n"[..];
        assert!(matches!(request.read_from(&mut pipelined, &ACCESS).unwrap(), Input::Rejected(Rejection::BadRequest)));
    }

    #[test]
    fn sends_exactly_one_response_across_short_writes() {
        #[derive(Default)]
        struct ShortWriter(Vec<u8>);
        impl Write for ShortWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.write(&bytes[..bytes.len().min(16)])
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut response = Response::asset("text/plain", b"a fixed response body", "");
        let mut output = ShortWriter::default();
        for _ in 0..(response.header.len() + response.body.as_ref().len()).div_ceil(16) + 1 {
            if response.write_to(&mut output).unwrap() {
                break;
            }
        }
        let header_end = response.header.len();
        assert_eq!(&output.0[..header_end], response.header.as_bytes());
        assert_eq!(&output.0[header_end..], b"a fixed response body");
    }

    #[test]
    fn only_the_document_allows_hashed_inline_scripts() {
        let index = Asset::at("/").unwrap().response();
        assert!(index.header.contains("Content-Type: text/html; charset=utf-8\r\n"));
        assert!(index.header.contains("script-src 'self' 'sha256-"));
        assert!(!index.header.contains("unsafe-inline"));
        assert!(!index.header.contains("unsafe-eval"));

        let image = Asset::at("/favicon.png").unwrap().response();
        assert!(image.header.contains("Content-Type: image/png\r\n"));
        assert!(image.header.contains("script-src 'self';"));
        assert!(!image.header.contains("sha256-"));
    }

    #[test]
    fn error_and_close_have_restrictive_headers() {
        let error = Response::bad_request("quotes \" backslash \\ newline\n");
        assert_eq!(error.body.as_ref(), br#"{"error":"quotes \" backslash \\ newline\u000a"}"#);
        for response in [error, Response::no_content()] {
            assert!(response.header.contains("Cache-Control: no-store\r\n"));
            assert!(response.header.contains("Content-Security-Policy: default-src 'none';"));
            assert!(response.header.contains("Cross-Origin-Opener-Policy: same-origin\r\n"));
            assert!(response.header.contains("Connection: close\r\n"));
            assert!(!response.header.contains("Access-Control-Allow-"));
        }
    }
}
