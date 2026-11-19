//! Edits only the top-level status line; the rest of the file is never reprinted.

use std::{fmt::Write as _, ops::Range};

use crate::json;

const NAME: &str = "statusLine";
pub(super) const LEGACY_COMMAND: &str = "milo claude-status";

/// A new settings file uses Claude Code's indentation and trailing newline.
pub(super) fn new(command: &str) -> String {
    let command = json_string(command);
    format!("{{\n  \"statusLine\": {{\n    \"type\": \"command\",\n    \"command\": {command}\n  }}\n}}\n")
}

/// Encode the shell command as a JSON string without changing its contents.
fn json_string(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len() + 2);
    encoded.push('"');
    for ch in text.chars() {
        match ch {
            '"' | '\\' => encoded.extend(['\\', ch]),
            '\0'..='\u{1f}' => write!(encoded, "\\u{:04x}", u32::from(ch)).expect("writing to a string"),
            _ => encoded.push(ch),
        }
    }
    encoded.push('"');
    encoded
}

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
    command: Option<json::Value<'a>>,
    removal: Option<Range<usize>>,
}

impl<'a> Settings<'a> {
    /// Reject ambiguous duplicate entries before setup might edit either one.
    pub(super) fn parse(text: &'a str) -> Option<Self> {
        let mut result = Self { text, open: 0, first: None, command: None, removal: None };
        let (mut previous, mut found, mut duplicate): (Option<Range<usize>>, _, _) = (None, false, false);
        result.open = json::settings_object(text, |member| {
            let start = member.key_range.start;
            let end = member.value_range.end;
            result.first.get_or_insert((start, &text[member.key_range.end..member.value_range.start]));
            if member.key.is(NAME) {
                duplicate |= result.removal.is_some();
                result.command = status_command(&text[member.value_range]);
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

    pub(super) fn status(&self, owned_command: Option<&str>) -> Status {
        if self.removal.is_none() {
            Status::Absent
        } else if self.command.is_some_and(|command| match owned_command {
            Some(owned) => command.is(owned),
            None => {
                command.is(LEGACY_COMMAND) || command.string(self.text.len()).is_some_and(|value| is_launcher(&value))
            }
        }) {
            Status::Milo
        } else {
            Status::Foreign
        }
    }

    /// Match the existing first member's spacing, if one is available.
    pub(super) fn add(&self, command: &str) -> Option<String> {
        if self.removal.is_some() {
            return None;
        }
        let command = json_string(command);
        let value = format!(r#"{{ "type": "command", "command": {command} }}"#);
        if let Some((first, colon)) = self.first {
            let prefix = &self.text[self.open..first];
            Some(format!("{}\"{NAME}\"{colon}{value},{prefix}{}", &self.text[..first], &self.text[first..]))
        } else {
            Some(format!("{}\"{NAME}\": {value}{}", &self.text[..self.open], &self.text[self.open..]))
        }
    }

    /// Remove the appropriate comma with milo's own entry, and no other bytes.
    pub(super) fn remove(&self, owned_command: &str) -> Option<String> {
        let range = self.removal.as_ref()?;
        (self.status(Some(owned_command)) == Status::Milo)
            .then(|| format!("{}{}", &self.text[..range.start], &self.text[range.end..]))
    }
}

/// Anything beyond the two fields we install belongs to the user.
fn status_command(text: &str) -> Option<json::Value<'_>> {
    let (mut count, mut command, mut kind) = (0, None, false);
    json::settings_object(text, |member| {
        count += 1;
        if member.key.is("command") {
            command = Some(member.value);
        }
        kind |= member.key.is("type") && member.value.is("command");
    })?;
    if count == 2 && kind { command } else { None }
}

/// Recognize a shell-quoted absolute launcher, not arbitrary shell code.
pub(super) fn is_launcher(command: &str) -> bool {
    command
        .strip_prefix("'/")
        .and_then(|path| path.strip_suffix("' claude-status"))
        .is_some_and(|path| !path.is_empty() && path.split("'\\''").all(|part| !part.contains('\'')))
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
            let command = "'/a b/quote'\\''back\\slash/milo' claude-status";
            let added = Settings::parse(original).unwrap().add(command).unwrap();
            let edited = Settings::parse(&added).unwrap();
            assert_eq!(edited.status(Some(command)), Status::Milo);
            assert_eq!(edited.status(None), Status::Milo);
            assert_eq!(edited.remove(command).as_deref(), Some(original));
        }
    }

    #[test]
    fn removal_only_touches_milos_member_even_when_it_has_moved() {
        let first = r#""status\u004cine":{"command":"milo claude-status", "type":"command"}"#;
        for (input, expected) in [
            (format!(r#"{{"a":1, {first}}}"#), r#"{"a":1}"#),
            (format!(r#"{{"a":1, {first}, "b":2}}"#), r#"{"a":1, "b":2}"#),
        ] {
            assert_eq!(Settings::parse(&input).unwrap().remove(LEGACY_COMMAND).as_deref(), Some(expected));
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
            assert_eq!(settings.status(None), Status::Foreign);
            assert!(settings.add(LEGACY_COMMAND).is_none() && settings.remove(LEGACY_COMMAND).is_none());
        }
    }

    #[test]
    fn editing_preserves_escapes_that_a_report_cannot_decode() {
        let original = r#"{"other":"\uD800"}"#;
        assert!(json::Value::parse(original.as_bytes()).is_none());
        let added = Settings::parse(original).unwrap().add(LEGACY_COMMAND).unwrap();
        assert_eq!(Settings::parse(&added).unwrap().remove(LEGACY_COMMAND).as_deref(), Some(original));
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
