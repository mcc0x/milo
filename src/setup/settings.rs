//! Edits only the top-level status line; the rest of the file is never reprinted.

use std::ops::Range;

const NAME: &str = "statusLine";
const VALUE: &str = r#"{ "type": "command", "command": "milo claude-status" }"#;

/// A new settings file uses Claude Code's indentation and trailing newline.
pub(super) const NEW: &str = concat!(
    "{\n",
    "  \"statusLine\": {\n",
    "    \"type\": \"command\",\n",
    "    \"command\": \"milo claude-status\"\n",
    "  }\n",
    "}\n",
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Absent,
    Milo,
    Foreign,
}

pub(super) struct Settings<'a> {
    text: &'a str,
    open: usize,
    first: Option<(usize, &'a str)>,
    status: Status,
    removal: Option<Range<usize>>,
}

impl<'a> Settings<'a> {
    pub(super) fn parse(text: &'a str) -> Option<Self> {
        let mut json = Json { text, at: 0 };
        json.space();
        let mut result = Self { text, open: json.at + 1, first: None, status: Status::Absent, removal: None };
        let (mut previous, mut found, mut duplicate): (Option<Range<usize>>, _, _) = (None, false, false);
        json.object(0, &mut |member| {
            let start = member.key.start;
            let end = member.value.end;
            result.first.get_or_insert((start, &text[member.key.end..member.value.start]));
            if quoted_eq(&text[member.key], NAME) {
                duplicate |= result.status != Status::Absent;
                result.status = if is_milo(&text[member.value]) { Status::Milo } else { Status::Foreign };
                result.removal = Some(previous.as_ref().map_or(start, |member| member.end)..end);
                found = true;
            } else if found {
                result.removal = Some(previous.as_ref().expect("status line was just visited").start..start);
                found = false;
            }
            // Keep the key start for removal with a next member, and the value end for a last member.
            previous = Some(start..end);
        })?;
        json.space();
        (!duplicate && json.at == text.len()).then_some(result)
    }

    pub(super) fn status(&self) -> Status {
        self.status
    }

    pub(super) fn add(&self) -> Option<String> {
        if self.status != Status::Absent {
            return None;
        }
        if let Some((first, colon)) = self.first {
            let prefix = &self.text[self.open..first];
            Some(format!("{}\"{NAME}\"{colon}{VALUE},{prefix}{}", &self.text[..first], &self.text[first..]))
        } else {
            Some(format!("{}\"{NAME}\": {VALUE}{}", &self.text[..self.open], &self.text[self.open..]))
        }
    }

    pub(super) fn remove(&self) -> Option<String> {
        let range = self.removal.as_ref()?;
        (self.status == Status::Milo).then(|| format!("{}{}", &self.text[..range.start], &self.text[range.end..]))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.first.is_none()
    }
}

fn is_milo(text: &str) -> bool {
    let mut json = Json { text, at: 0 };
    let (mut count, mut command, mut kind) = (0, false, false);
    json.object(0, &mut |member| {
        count += 1;
        let (key, value) = (&text[member.key], &text[member.value]);
        command |= quoted_eq(key, "command") && quoted_eq(value, "milo claude-status");
        kind |= quoted_eq(key, "type") && quoted_eq(value, "command");
    })
    .is_some()
        && count == 2
        && command
        && kind
}

/// Compare a validated JSON string to an ASCII name without decoding or copying it.
fn quoted_eq(text: &str, expected: &str) -> bool {
    let Some(contents) = text.strip_prefix('"').and_then(|text| text.strip_suffix('"')) else { return false };
    let mut bytes = contents.as_bytes();
    for want in expected.bytes() {
        if let Some(hex) = bytes.strip_prefix(b"\\u") {
            let Some(digits) = hex.first_chunk::<4>() else { return false };
            if std::str::from_utf8(digits).ok().and_then(|s| u16::from_str_radix(s, 16).ok()) != Some(u16::from(want)) {
                return false;
            }
            bytes = &hex[4..];
        } else if let Some(rest) = bytes.strip_prefix(&[want]) {
            bytes = rest;
        } else {
            return false;
        }
    }
    bytes.is_empty()
}

struct Member {
    key: Range<usize>,
    value: Range<usize>,
}

/// Validates JSON syntax and identifies byte ranges. Unknown values are never materialized.
struct Json<'a> {
    text: &'a str,
    at: usize,
}

impl Json<'_> {
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn take(&mut self, byte: u8) -> bool {
        let found = self.byte() == Some(byte);
        self.at += usize::from(found);
        found
    }

    fn space(&mut self) {
        while matches!(self.byte(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn string(&mut self) -> Option<Range<usize>> {
        let start = self.at;
        self.take(b'"').then_some(())?;
        loop {
            let byte = self.byte()?;
            self.at += 1;
            match byte {
                b'"' => return Some(start..self.at),
                b'\\' => {
                    let escape = self.byte()?;
                    self.at += 1;
                    match escape {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                        b'u' => {
                            let hex = self.text.as_bytes().get(self.at..self.at + 4)?;
                            hex.iter().all(u8::is_ascii_hexdigit).then_some(())?;
                            self.at += 4;
                        }
                        _ => return None,
                    }
                }
                0..=31 => return None,
                _ => {}
            }
        }
    }

    fn digits(&mut self) -> Option<()> {
        let start = self.at;
        while matches!(self.byte(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        (self.at > start).then_some(())
    }

    fn number(&mut self) -> Option<()> {
        self.take(b'-');
        if !self.take(b'0') {
            self.digits()?;
        }
        if self.take(b'.') {
            self.digits()?;
        }
        if self.take(b'e') || self.take(b'E') {
            if !self.take(b'+') {
                self.take(b'-');
            }
            self.digits()?;
        }
        Some(())
    }

    fn literal(&mut self, value: &str) -> Option<()> {
        self.text.as_bytes().get(self.at..self.at + value.len()).filter(|bytes| *bytes == value.as_bytes())?;
        self.at += value.len();
        Some(())
    }

    fn value(&mut self, depth: usize) -> Option<Range<usize>> {
        self.space();
        let start = self.at;
        if depth > 128 {
            return None;
        }
        match self.byte()? {
            b'{' => self.object(depth, &mut |_| {})?,
            b'[' => self.array(depth)?,
            b'"' => {
                self.string()?;
            }
            b't' => self.literal("true")?,
            b'f' => self.literal("false")?,
            b'n' => self.literal("null")?,
            b'-' | b'0'..=b'9' => self.number()?,
            _ => return None,
        }
        Some(start..self.at)
    }

    fn object(&mut self, depth: usize, visit: &mut dyn FnMut(Member)) -> Option<()> {
        self.take(b'{').then_some(())?;
        self.space();
        if self.take(b'}') {
            return Some(());
        }
        loop {
            self.space();
            let key = self.string()?;
            self.space();
            self.take(b':').then_some(())?;
            let value = self.value(depth + 1)?;
            visit(Member { key, value });
            self.space();
            if self.take(b'}') {
                return Some(());
            }
            self.take(b',').then_some(())?;
        }
    }

    fn array(&mut self, depth: usize) -> Option<()> {
        self.take(b'[').then_some(())?;
        self.space();
        if self.take(b']') {
            return Some(());
        }
        loop {
            self.value(depth + 1)?;
            self.space();
            if self.take(b']') {
                return Some(());
            }
            self.take(b',').then_some(())?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_the_status_line_round_trips_other_keys_and_whitespace() {
        for original in [
            "{}",
            "{\n}\n",
            "{\r\n\t\"a\" : 18,\r\n\t\"b\":null\r\n}\r\n",
            r#"{"first":{"nested":[1,-2.1e5,{"statusLine":null}]}, "text":"} {, \"quoted\" \\ \u1234"}"#,
        ] {
            let added = Settings::parse(original).unwrap().add().unwrap();
            let edited = Settings::parse(&added).unwrap();
            assert_eq!(edited.status(), Status::Milo);
            assert_eq!(edited.remove().as_deref(), Some(original));
        }
    }

    #[test]
    fn removal_only_touches_milos_member_even_when_it_has_moved() {
        let first = r#""status\u004cine":{"command":"milo claude-status", "type":"command"}"#;
        for (input, expected) in [
            (format!(r#"{{"a":1, {first}}}"#), r#"{"a":1}"#),
            (format!(r#"{{"a":1, {first}, "b":2}}"#), r#"{"a":1, "b":2}"#),
        ] {
            assert_eq!(Settings::parse(&input).unwrap().remove().as_deref(), Some(expected));
        }
    }

    #[test]
    fn foreign_status_lines_are_never_replaced_or_removed() {
        for value in [
            "null",
            r#"{"type":"command","command":"another-program"}"#,
            r#"{"type":"command","command":"milo claude-status","padding":1}"#,
        ] {
            let text = format!(r#"{{"status\u004cine":{value}}}"#);
            let settings = Settings::parse(&text).unwrap();
            assert_eq!(settings.status(), Status::Foreign);
            assert!(settings.add().is_none() && settings.remove().is_none());
        }
    }

    #[test]
    fn unsafe_settings_are_not_editable() {
        for text in [
            "[]",
            "null",
            "{} true",
            "{\"a\": 01}",
            "{\"a\": 1.}",
            "{\"a\": [1,]}",
            "{\"a\": true,}",
            r#"{"a":"\uXXXX"}"#,
            r#"{"statusLine":null,"status\u004cine":true}"#,
        ] {
            assert!(Settings::parse(text).is_none(), "{text}");
        }
    }
}
