//! Resolves the fresh label of an alert's exact session, never its current tab.

use std::borrow::Cow;
use std::ffi::OsStr;

use super::event::{Envelope, TERMINAL};
use crate::{json::Value, rex};

const LABEL_LIMIT: usize = 64;
const REX_LABEL_LIMIT: usize = 128;

pub(super) fn label(event: &Envelope<'_>, host: &str, lookup: impl FnOnce(&str) -> Option<Vec<u8>>) -> Option<String> {
    if !event.kind.is("block_event") {
        return None;
    }
    let [block, creator] = event.payload.fields(["block_id", "creator_name"]);
    if !creator?.is(TERMINAL) {
        return None;
    }
    let session = exact(event.session?, "session:")?;
    let block = exact(block?, "block:")?;
    let view = lookup(&session)?;
    from_view(&view, host, &session, &block)
}

fn exact<'a>(value: Value<'a>, kind: &str) -> Option<Cow<'a, str>> {
    let id = value.string(kind.len() + 36)?;
    rex::exact_id(OsStr::new(id.as_ref()), kind)?;
    Some(id)
}

fn from_view(view: &[u8], host: &str, session: &str, block: &str) -> Option<String> {
    let [id, session_label, windows] = Value::parse(view)?.fields(["session_id", "label", "windows"]);
    if !id?.is(session) {
        return None;
    }
    // Verify membership wherever the block is now. The event does not say
    // which tab it came from, and the block may already have moved tabs.
    let mut found = false;
    for window in windows?.elements()? {
        let [layers] = window.fields(["layers"]);
        for layer in layers?.elements()? {
            let [blocks] = layer.fields(["blocks"]);
            for member in blocks?.elements()? {
                let [id, creator, member_host] = member.fields(["block_id", "creator_name", "host_id"]);
                if id?.is(block) {
                    if found || !creator?.is(TERMINAL) || !member_host?.is(host) {
                        return None;
                    }
                    found = true;
                }
            }
        }
    }
    found.then_some(())?;
    label_text(session_label?.string(REX_LABEL_LIMIT)?)
}

fn label_text(label: Cow<'_, str>) -> Option<String> {
    // Recognize only `project[/worktree] · agent` in the verified session's
    // structured label. Disclose one slug, not a path or a terminal title.
    if let Some((checkout, agent)) = label.split_once(" · ") {
        if !matches!(agent, "claude" | "codex" | "omp") {
            return None;
        }
        let (project, worktree) = checkout.split_once('/').unwrap_or((checkout, checkout));
        return (slug(project) && slug(worktree)).then(|| worktree.to_owned());
    }
    plain_label(label)
}

fn slug(label: &str) -> bool {
    alnum_ends(label) && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Reject controls, formatting, URLs and paths rather than repairing them into
/// a different name. The same plain label can be rendered by both destinations.
fn plain_label(label: Cow<'_, str>) -> Option<String> {
    (alnum_ends(&label) && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'_' | b'-')))
        .then(|| label.into_owned())
}

fn alnum_ends(label: &str) -> bool {
    label.len() <= LABEL_LIMIT
        && label.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && label.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "session:01a20936-aae6-7222-9946-da582c64d27d";
    const BLOCK: &str = "block:01a20936-aae6-73ff-959e-105c1771ad18";
    const TAB: &str = "window:01a20936-aaf5-7943-b27f-62c85a2cfb97";
    const FOCUSED: &str = "window:01a20936-ab86-7a9e-b61a-35bfe53c822d";
    const HOST: &str = "cf061360-76f6-4bf1-8643-5e347b338fe0";

    fn frame(session: &str, kind: &str, payload: &str) -> String {
        format!(
            r#"{{"type":"session_event","payload":{{"session_id":"{session}","event":{{"type":"{kind}","payload":{payload}}}}}}}"#
        )
    }

    fn bell(session: &str, block: &str) -> String {
        frame(
            session,
            "block_event",
            &format!(r#"{{"block_id":"{block}","creator_name":"{TERMINAL}","name":"bell","payload":{{}}}}"#),
        )
    }

    fn view(session_label: &str, moved: bool) -> String {
        let member = format!(r#"{{"block_id":"{BLOCK}","creator_name":"{TERMINAL}","host_id":"{HOST}"}}"#);
        let (first, second) = if moved { (member.as_str(), "") } else { ("", member.as_str()) };
        format!(
            r#"{{"session_id":"{SESSION}","label":"{session_label}",
                "active_window_id":"{FOCUSED}","windows":[
                    {{"window_id":"{FOCUSED}","label":"Other Tab","layers":[{{"blocks":[{first}]}}]}},
                    {{"window_id":"{TAB}","label":"Run Tab","layers":[{{"blocks":[{second}]}}]}}
                ]}}"#
        )
    }

    fn resolve(reply: String) -> Option<String> {
        label(&Envelope::read(bell(SESSION, BLOCK).as_bytes()).unwrap(), HOST, |id| {
            assert_eq!(id, SESSION);
            Some(reply.into_bytes())
        })
    }

    #[test]
    fn a_move_or_rename_resolves_only_the_events_still_live_session() {
        let frame = bell(SESSION, BLOCK);
        let event = Envelope::read(frame.as_bytes()).unwrap();
        let lookup = |name, moved| label(&event, HOST, |_| Some(view(name, moved).into_bytes()));
        assert_eq!(lookup("Release Review", false).as_deref(), Some("Release Review"));
        // Even for the same earlier frame, a move within the session cannot
        // make the new tab its origin. A rename uses the fresh session name.
        assert_eq!(lookup("Release Review", true).as_deref(), Some("Release Review"));
        assert_eq!(lookup("New Session Name", true).as_deref(), Some("New Session Name"));
        assert_eq!(lookup("/unsafe-session", true), None);
        assert_eq!(label(&event, HOST, |_| None), None);
    }

    #[test]
    fn mismatched_closed_or_duplicate_identities_get_no_label() {
        let current = view("Release Review", false);
        for mismatch in [
            current.replace(SESSION, "session:01a20936-aae6-7222-9946-da582c64d27c"),
            current.replace(BLOCK, "block:01a20936-aae6-73ff-959e-105c1771ad19"),
            current.replace(HOST, "cf061360-76f6-4bf1-8643-5e347b338fe1"),
            current.replace(TERMINAL, "com.example.plugin"),
            current.replace(
                r#""blocks":[]"#,
                &format!(r#""blocks":[{{"block_id":"{BLOCK}","creator_name":"{TERMINAL}","host_id":"{HOST}"}}]"#),
            ),
        ] {
            assert_eq!(resolve(mismatch), None);
        }
        for (session, block) in [("Release Review", BLOCK), (SESSION, "105c1771ad18"), (SESSION, "--help")] {
            let frame = bell(session, block);
            let event = Envelope::read(frame.as_bytes()).unwrap();
            assert_eq!(label(&event, HOST, |_| panic!("partial ID used for lookup")), None);
        }
    }

    #[test]
    fn only_bounded_structured_session_labels_and_known_title_forms_are_disclosed() {
        for (session, expected) in [
            ("Release_Review-2", Some("Release_Review-2")),
            ("project · omp", Some("project")),
            ("project/worktree · codex", Some("worktree")),
            ("secret/worktree · not-ours", None),
            ("/private/file · claude", None),
            ("project/worktree", None),
            ("relative/path/deeper · claude", None),
            ("curl https://example.com", None),
            ("secret\\u001b", None),
            ("no\\u202ebidi", None),
            ("@everyone", None),
            ("`markup`", None),
            (" Leading", None),
            ("", None),
        ] {
            assert_eq!(resolve(view(session, false)).as_deref(), expected, "{session}");
        }
        let longest = "a".repeat(LABEL_LIMIT);
        assert_eq!(resolve(view(&longest, false)).as_deref(), Some(longest.as_str()));
        assert_eq!(resolve(view(&(longest + "a"), false)), None);
    }
}
