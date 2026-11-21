//! Turns a session's label and a project's folder name into the one extra
//! line Discord may show, when the person chose to show them.
//!
//! A name is shown only as one short line of ordinary text. Anything that
//! looks like a path or an address is left out whole, never trimmed into
//! something that might still give it away.

/// The most characters of a name that are shown.
const NAME_LIMIT: usize = 32;

/// A name fit to show in public, or nothing if it has none or could be a path or an address.
pub(super) fn display_name(raw: &str) -> Option<String> {
    let mut name = String::new();
    for word in raw.split(|c: char| c.is_whitespace() || unprintable(c)).filter(|word| !word.is_empty()) {
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(word);
    }
    // A folder separator means a path or a URL; `user@host:` means a remote address.
    let address = name.split_once('@').is_some_and(|(_, host)| host.contains(':'));
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('~') || address {
        return None;
    }
    if name.chars().count() > NAME_LIMIT {
        name = name.chars().take(NAME_LIMIT - 1).collect();
        name.truncate(name.trim_end().len());
        name.push('…');
    }
    Some(name)
}

/// Controls, and the marks that reorder or hide the text around them.
fn unprintable(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

/// The line under “In a Rex session”, from the names that are known and were chosen.
pub(super) fn line(session: Option<&str>, project: Option<&str>) -> Option<String> {
    match (project, session) {
        (Some(project), Some(session)) => Some(format!("Project: {project} · Session: {session}")),
        (Some(project), None) => Some(format!("Project: {project}")),
        (None, Some(session)) => Some(format!("Session: {session}")),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_shown_as_one_short_line_of_plain_text() {
        assert_eq!(display_name("release-notes").as_deref(), Some("release-notes"));
        assert_eq!(display_name("  fix\tlogin \n redirect ").as_deref(), Some("fix login redirect"));
        assert_eq!(display_name("a\u{1b}[31mb\u{7}").as_deref(), Some("a [31mb"));
        // Marks that would reorder the line around the name do not reach Discord.
        assert_eq!(display_name("safe\u{202e}txt.exe\u{2066}").as_deref(), Some("safe txt.exe"));
        assert_eq!(display_name("Ünïcode 猫").as_deref(), Some("Ünïcode 猫"));
        assert_eq!(display_name("say \"hi\"").as_deref(), Some("say \"hi\""));
        let long = display_name(&"x".repeat(80)).unwrap();
        assert_eq!(long.chars().count(), 32);
        assert!(long.ends_with('…'));
        assert_eq!(display_name(&"é".repeat(32)).unwrap(), "é".repeat(32));
    }

    #[test]
    fn a_name_that_could_be_a_path_or_an_address_is_left_out_whole() {
        for hidden in [
            "",
            " \t\n",
            "/Users/someone/secret-client",
            "secret-client/api",
            "~someone",
            "C:\\work\\client",
            "https://example.com/owner/repo",
            "git@example.com:owner/repo.git",
            "someone@host:22",
        ] {
            assert_eq!(display_name(hidden), None, "{hidden:?}");
        }
        // An address is the pair of marks together, not either on its own.
        assert_eq!(display_name("fix: mail to a@b").as_deref(), Some("fix: mail to a@b"));
    }

    #[test]
    fn the_line_holds_only_the_names_that_are_known() {
        assert_eq!(line(Some("release"), Some("milo")).as_deref(), Some("Project: milo · Session: release"));
        assert_eq!(line(Some("release"), None).as_deref(), Some("Session: release"));
        assert_eq!(line(None, Some("milo")).as_deref(), Some("Project: milo"));
        assert_eq!(line(None, None), None);
    }
}
