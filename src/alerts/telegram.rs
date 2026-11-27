use std::borrow::Cow;
use std::collections::BTreeMap;

use super::send;
use crate::json::Value;

pub(super) const API: &str = "https://api.telegram.org";
const UPDATE_LIMIT: usize = 100; // Telegram's default getUpdates limit.
const ANSWER_LIMIT: u64 = 2 << 20;
const NAME_LIMIT: usize = 256;

pub(super) fn discover(stand_in: Option<&str>, token: &str) -> Option<Vec<(i64, String)>> {
    let address = format!("{}/bot{token}/getUpdates", stand_in.unwrap_or(API));
    // With no offset, Telegram does not acknowledge any updates. Do not set
    // allowed_updates either: Telegram would remember it for future calls.
    let answer = send::query(&address, r#"{"timeout":0}"#, ANSWER_LIMIT)?;
    recent_chats(answer.as_bytes())
}

fn recent_chats(answer: &[u8]) -> Option<Vec<(i64, String)>> {
    let [ok, result] = Value::parse(answer)?.fields(["ok", "result"]);
    if !ok?.boolean()? {
        return None;
    }
    let mut chats = BTreeMap::new();
    for update in result?.elements()?.take(UPDATE_LIMIT) {
        let [message, edited, channel, edited_channel, membership] =
            update.fields(["message", "edited_message", "channel_post", "edited_channel_post", "my_chat_member"]);
        let Some(event) = message.or(edited).or(channel).or(edited_channel).or(membership) else { continue };
        let [chat, new_member] = event.fields(["chat", "new_chat_member"]);
        let Some(chat) = chat else { continue };
        let Some(id) = chat.fields(["id"])[0].and_then(Value::integer).filter(|&id| id != 0) else { continue };
        let removed = membership.is_some()
            && new_member.is_some_and(|member| {
                let [status, is_member] = member.fields(["status", "is_member"]);
                status.is_some_and(|status| {
                    status.is("left")
                        || status.is("kicked")
                        || (status.is("restricted") && is_member.and_then(Value::boolean) == Some(false))
                })
            });
        if removed {
            chats.remove(&id);
        } else {
            // Later updates carry newer names for the same chat.
            chats.insert(id, chat);
        }
    }
    Some(chats.into_iter().map(|(id, chat)| (id, title(chat, id))).collect())
}

fn title(chat: Value<'_>, id: i64) -> String {
    let [title, first, last, username] = chat.fields(["title", "first_name", "last_name", "username"]);
    if let Some(title) = name(title) {
        return title.into_owned();
    }
    match (name(first), name(last)) {
        (Some(first), Some(last)) => format!("{first} {last}"),
        (Some(part), None) | (None, Some(part)) => part.into_owned(),
        (None, None) => name(username).map_or_else(|| id.to_string(), |name| format!("@{name}")),
    }
}

fn name(value: Option<Value<'_>>) -> Option<Cow<'_, str>> {
    value?.string(NAME_LIMIT).filter(|name| !name.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_updates_discover_private_chats_groups_and_channels_without_message_content() {
        let answer = br#"{"ok":true,"result":[
            {"update_id":1,"message":{"chat":{"id":42,"first_name":"Ada","last_name":"Lovelace"},"text":"private"}},
            {"update_id":2,"channel_post":{"chat":{"id":-1001234567890,"title":"Milo \"News\""}}},
            {"update_id":3,"edited_message":{"chat":{"id":-19,"username":"milo_group"}}},
            {"update_id":4,"edited_channel_post":{"chat":{"id":-99}}},
            {"update_id":5,"my_chat_member":{"chat":{"id":-27,"title":"Team"},"new_chat_member":{"status":"member"}}},
            {"update_id":6,"inline_query":{"from":{"id":99}}},
            {"update_id":7,"message":{"chat":{"id":"12345","title":"not a numeric ID"}}}
        ]}"#;
        assert_eq!(
            recent_chats(answer).unwrap(),
            [
                (-1001234567890, "Milo \"News\""),
                (-99, "-99"),
                (-27, "Team"),
                (-19, "@milo_group"),
                (42, "Ada Lovelace")
            ]
            .map(|(id, title)| (id, title.to_owned()))
        );
    }

    #[test]
    fn recent_updates_use_one_latest_name_per_chat_and_omit_chats_the_bot_left() {
        let answer = br#"{"ok":true,"result":[
            {"message":{"chat":{"id":-100,"title":"Old name"}}},
            {"my_chat_member":{"chat":{"id":-100,"title":"New name"},"new_chat_member":{"status":"administrator"}}},
            {"message":{"chat":{"id":-42,"title":"Old group"}}},
            {"my_chat_member":{"chat":{"id":-42,"title":"Old group"},"new_chat_member":{"status":"kicked"}}},
            {"channel_post":{"chat":{"id":-123,"title":"Channel"}}},
            {"edited_channel_post":{"chat":{"id":-123,"title":"Channel"}}}
        ]}"#;
        assert_eq!(recent_chats(answer).unwrap(), [(-123, "Channel".to_owned()), (-100, "New name".to_owned())]);
    }

    #[test]
    fn chat_count_and_title_sizes_are_bounded() {
        use std::fmt::Write as _;

        let long_title = "x".repeat(NAME_LIMIT + 1);
        let mut answer = String::from(r#"{"ok":true,"result":["#);
        for id in 1..=UPDATE_LIMIT + 1 {
            if id != 1 {
                answer.push(',');
            }
            write!(answer, r#"{{"message":{{"chat":{{"id":{id},"title":"{long_title}"}}}}}}"#).unwrap();
        }
        answer.push_str("]}");
        let chats = recent_chats(answer.as_bytes()).unwrap();
        assert_eq!(chats.len(), UPDATE_LIMIT);
        assert_eq!(chats.first().map(|(id, title)| (*id, title.as_str())), Some((1, "1")));
    }

    #[test]
    fn empty_results_are_not_upstream_failures() {
        assert_eq!(recent_chats(br#"{"ok":true,"result":[]}"#), Some(vec![]));
        for answer in [r#"{"ok":false,"description":"Telegram error"}"#, r#"{"ok":true,"result":{}}"#, "not JSON"] {
            assert_eq!(recent_chats(answer.as_bytes()), None);
        }
    }
}
