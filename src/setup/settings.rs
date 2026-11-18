//! Edits only the top-level status line; the rest of the file is never reprinted.

use std::ops::Range;

use crate::json;

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

/// Keep spans instead of parsed values so unrelated settings can remain exactly as written.
pub(super) struct Settings<'a> {
    text: &'a str,
    open: usize,
    first: Option<(usize, &'a str)>,
    status: Status,
    removal: Option<Range<usize>>,
}

impl<'a> Settings<'a> {
    /// Reject ambiguous duplicate entries before setup might edit either one.
    pub(super) fn parse(text: &'a str) -> Option<Self> {
        let mut result = Self { text, open: 0, first: None, status: Status::Absent, removal: None };
        let (mut previous, mut found, mut duplicate): (Option<Range<usize>>, _, _) = (None, false, false);
        result.open = json::settings_object(text, |member| {
            let start = member.key_range.start;
            let end = member.value_range.end;
            result.first.get_or_insert((start, &text[member.key_range.end..member.value_range.start]));
            if member.key.is(NAME) {
                duplicate |= result.status != Status::Absent;
                result.status = if is_milo(&text[member.value_range]) { Status::Milo } else { Status::Foreign };
                result.removal = Some(previous.as_ref().map_or(start, |member| member.end)..end);
                found = true;
            } else if found {
                result.removal = Some(previous.as_ref().expect("status line was just visited").start..start);
                found = false;
            }
            // Keep the key start for removal with a next member, and the value end for a last member.
            previous = Some(start..end);
        })?;
        (!duplicate).then_some(result)
    }

    pub(super) fn status(&self) -> Status {
        self.status
    }

    /// Match the existing first member's spacing, if one is available.
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

    /// Remove the appropriate comma with milo's own entry, and no other bytes.
    pub(super) fn remove(&self) -> Option<String> {
        let range = self.removal.as_ref()?;
        (self.status == Status::Milo).then(|| format!("{}{}", &self.text[..range.start], &self.text[range.end..]))
    }
}

/// Anything beyond the two fields we install belongs to the user.
fn is_milo(text: &str) -> bool {
    let (mut count, mut command, mut kind) = (0, false, false);
    json::settings_object(text, |member| {
        count += 1;
        command |= member.key.is("command") && member.value.is("milo claude-status");
        kind |= member.key.is("type") && member.value.is("command");
    })
    .is_some()
        && count == 2
        && command
        && kind
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
    fn editing_preserves_escapes_that_a_report_cannot_decode() {
        let original = r#"{"other":"\uD800"}"#;
        assert!(json::Value::parse(original.as_bytes()).is_none());
        let added = Settings::parse(original).unwrap().add().unwrap();
        assert_eq!(Settings::parse(&added).unwrap().remove().as_deref(), Some(original));
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
