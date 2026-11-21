//! The local hosted-connect prototype. None of its addresses are user settings.

use std::borrow::Cow;

use super::{Destination, send, webhook};
use crate::json::Value;
use crate::rex::json;

const ORIGIN: &str = "http://127.0.0.1:6120";
pub(crate) const REQUEST_LIMIT: usize = 1024;
const RESPONSE_LIMIT: u64 = 4096;
const KEY_LEN: usize = 43;
const PREFIX: &str = "milorelay_";

pub(super) fn valid_capability(secret: &str) -> bool {
    secret.strip_prefix(PREFIX).is_some_and(valid_key)
}

fn valid_key(key: &str) -> bool {
    key.len() == KEY_LEN && key.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

pub(super) fn valid_chat(chat: &str) -> bool {
    !chat.starts_with('0') && chat.bytes().all(|byte| byte.is_ascii_digit()) && chat.parse::<i64>().is_ok()
}

pub(super) fn send_request(origin: Option<&str>, capability: &str, text: &str) -> String {
    let address = format!("{}/v1/send", origin.unwrap_or(ORIGIN));
    let body = format!("{{\"text\":{}}}", json(text));
    send::relay_request(&address, "POST", Some(&body), Some(capability))
}

/// The relay returns 401 when this syntactically valid capability is no longer bound.
pub(super) fn revoke(origin: Option<&str>, capability: &str) -> bool {
    let address = format!("{}/v1/binding", origin.unwrap_or(ORIGIN));
    let request = send::relay_request(&address, "DELETE", None, Some(capability));
    matches!(send::response(&request, RESPONSE_LIMIT, None), Some((_, 204 | 401)))
}

pub(crate) enum Connected {
    Ready(String),
    Pending,
}

pub(crate) enum ConnectError {
    Invalid,
    Expired,
    Busy,
    Disabled,
    Unavailable,
}

pub(crate) fn connect(body: &[u8]) -> Result<Connected, ConnectError> {
    connect_at(ORIGIN, body)
}

enum Pairing<'a> {
    Start(Destination),
    Claim(Cow<'a, str>),
}

fn connect_at(origin: &str, body: &[u8]) -> Result<Connected, ConnectError> {
    use ConnectError::{Busy, Disabled, Expired, Invalid, Unavailable};
    let operation = Pairing::parse(body).ok_or(Invalid)?;
    let (path, body) = match &operation {
        Pairing::Start(provider) => ("pair", format!("{{\"provider\":\"{}\"}}", provider.name())),
        Pairing::Claim(key) => ("claim", format!("{{\"deviceKey\":\"{key}\"}}")),
    };
    let request = send::relay_request(&format!("{origin}/v1/{path}"), "POST", Some(&body), None);
    let (reply, status) = send::response(&request, RESPONSE_LIMIT, None).ok_or(Unavailable)?;
    match (operation, status) {
        (Pairing::Start(provider), 200) => paired(provider, &reply).map(Connected::Ready).ok_or(Unavailable),
        (Pairing::Claim(_), 200) => claimed(&reply).map(Connected::Ready).ok_or(Unavailable),
        (Pairing::Claim(_), 202) => match fields(&reply, ["pending"]) {
            Some([Some(value)]) if value.boolean() == Some(true) => Ok(Connected::Pending),
            _ => Err(Unavailable),
        },
        (Pairing::Claim(_), 404) => Err(Expired),
        (_, 429) => Err(Busy),
        (_, 503) => Err(Disabled),
        _ => Err(Unavailable),
    }
}

impl<'a> Pairing<'a> {
    fn parse(body: &'a [u8]) -> Option<Self> {
        if body.len() > REQUEST_LIMIT {
            return None;
        }
        let [action, provider, key] = fields(std::str::from_utf8(body).ok()?, ["action", "provider", "deviceKey"])?;
        match (action?.string(5)?.as_ref(), provider, key) {
            ("start", Some(provider), None) => Some(Self::Start(Destination::parse(&provider.string(8)?)?)),
            ("claim", None, Some(key)) => {
                let key = key.string(KEY_LEN)?;
                valid_key(&key).then_some(Self::Claim(key))
            }
            _ => None,
        }
    }
}

/// Validate and reconstruct success replies. Don't forward arbitrary upstream fields or error text.
fn paired(provider: Destination, reply: &str) -> Option<String> {
    let [key, url] = fields(reply, ["deviceKey", "url"])?;
    let key = key?.string(KEY_LEN)?;
    let url = url?.string(256)?;
    let public_key = match provider {
        Destination::Telegram => {
            let (bot, start) = url.strip_prefix("https://t.me/")?.split_once("?start=")?;
            if !(5..=32).contains(&bot.len()) || !bot.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return None;
            }
            start
        }
        Destination::Discord => url
            .strip_prefix("http://127.0.0.1:6120/discord/start/")
            .or_else(|| url.strip_prefix("http://localhost:6120/discord/start/"))?,
    };
    // The browser link is not authority to claim and obtain the credential.
    (valid_key(&key) && valid_key(public_key) && public_key != key)
        .then(|| format!("{{\"deviceKey\":\"{key}\",\"url\":\"{url}\"}}"))
}

fn claimed(reply: &str) -> Option<String> {
    let [provider, label, credential, chat] = fields(reply, ["provider", "label", "credential", "chat"])?;
    let provider = Destination::parse(&provider?.string(8)?)?;
    let credential = credential?.string(256)?;
    let chat = chat?.string(19)?;
    let label = label?.string(40)?;
    let valid = match provider {
        Destination::Telegram => {
            valid_capability(&credential)
                && valid_chat(&chat)
                && label.strip_prefix("Telegram DM · ") == Some(chat.as_ref())
        }
        Destination::Discord => {
            webhook(&credential).is_some()
                && chat.is_empty()
                && label.strip_prefix("Discord channel · ").is_some_and(|id| {
                    !id.starts_with('0') && id.bytes().all(|byte| byte.is_ascii_digit()) && id.parse::<u64>().is_ok()
                })
        }
    };
    valid.then(|| {
        format!(
            "{{\"provider\":\"{}\",\"label\":\"{label}\",\"credential\":\"{credential}\",\"chat\":\"{chat}\"}}",
            provider.name(),
        )
    })
}

fn fields<'a, const N: usize>(object: &'a str, names: [&str; N]) -> Option<[Option<Value<'a>>; N]> {
    let mut fields = [None; N];
    let mut valid = true;
    crate::json::settings_object(object, |member| {
        valid &= names.iter().position(|name| member.key.is(name)).is_some_and(|index| {
            fields[index].replace(member.value).is_none()
        });
    })?;
    valid.then_some(fields)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::alerts::{Alerts, TelegramChatsError};
    use crate::fixture::Fixture;

    const DEVICE: &str = concat!("aaaaaaaaaa", "aaaaaaaaaa", "aaaaaaaaaa", "aaaaaaaaaa", "aaa");
    const LINK_KEY: &str = concat!("bbbbbbbbbb", "bbbbbbbbbb", "bbbbbbbbbb", "bbbbbbbbbb", "bbb");
    const CAPABILITY: &str = concat!("milorelay_", "0123456789", "abcdefghij", "ABCDEFGHIJ", "9876543210", "_-x");
    const MANUAL: &str = "123456:syntheticSYNTHETICsynthetic00";

    fn with_replies<T>(replies: &[(u16, &str)], act: impl FnOnce(&str) -> T) -> (T, Vec<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        thread::scope(|scope| {
            let fixture = scope.spawn(move || {
                replies
                    .iter()
                    .map(|(status, body)| {
                        let deadline = Instant::now() + Duration::from_secs(5);
                        let mut socket = loop {
                            match listener.accept() {
                                Ok((socket, _)) => break socket,
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    assert!(Instant::now() < deadline, "relay request not received");
                                    thread::sleep(Duration::from_millis(5));
                                }
                                Err(error) => panic!("{error}"),
                            }
                        };
                        socket.set_nonblocking(false).unwrap();
                        socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                        let mut reader = BufReader::new(&mut socket);
                        let mut wire = String::new();
                        while !wire.ends_with("\r\n\r\n") {
                            assert_ne!(reader.read_line(&mut wire).unwrap(), 0);
                        }
                        let len = wire.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("Content-Length").then(|| value.trim().parse::<u64>().unwrap())
                        });
                        (&mut reader).take(len.unwrap_or(0)).read_to_string(&mut wire).unwrap();
                        write!(
                            socket,
                            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len(),
                        )
                        .unwrap();
                        wire
                    })
                    .collect()
            });
            (act(&origin), fixture.join().unwrap())
        })
    }

    #[test]
    fn only_exact_bounded_pair_and_claim_operations_are_accepted() {
        for body in [
            r#"{"action":"send","provider":"telegram"}"#,
            r#"{"action":"start","provider":"slack"}"#,
            r#"{"action":"start","provider":"telegram","url":"http://example.org"}"#,
            r#"{"action":"start","action":"claim","provider":"telegram"}"#,
            r#"{"action":"claim","deviceKey":"anything"}"#,
            r#"{"action":"claim","deviceKey":true}"#,
        ] {
            assert!(Pairing::parse(body.as_bytes()).is_none(), "{body}");
        }
        let large = format!(r#"{{"action":"start","provider":"telegram"}}{}"#, " ".repeat(REQUEST_LIMIT));
        assert!(Pairing::parse(large.as_bytes()).is_none());
    }

    #[test]
    fn pairing_reconstructs_bounded_replies_and_never_forwards_browser_authority() {
        let start = br#"{"action":"start","provider":"telegram"}"#;
        let claim = format!(r#"{{"action":"claim","deviceKey":"{DEVICE}"}}"#);
        let pair = format!(r#"{{"deviceKey":"{DEVICE}","url":"https://t.me/MiloBot?start={LINK_KEY}"}}"#);
        let ready = format!(
            r#"{{"provider":"telegram","label":"Telegram DM · 42","credential":"{CAPABILITY}","chat":"42"}}"#,
        );
        let huge = "x".repeat(usize::try_from(RESPONSE_LIMIT).unwrap() + 1);
        let replies = [
            (200, pair.as_str()),
            (202, r#"{"pending":true}"#),
            (200, ready.as_str()),
            (404, MANUAL),
            (200, huge.as_str()),
        ];
        let (_, wire) = with_replies(&replies, |origin| {
            assert!(matches!(connect_at(origin, start), Ok(Connected::Ready(body)) if body == pair));
            assert!(matches!(connect_at(origin, claim.as_bytes()), Ok(Connected::Pending)));
            assert!(matches!(connect_at(origin, claim.as_bytes()), Ok(Connected::Ready(body)) if body == ready));
            assert!(matches!(connect_at(origin, claim.as_bytes()), Err(ConnectError::Expired)));
            assert!(matches!(connect_at(origin, claim.as_bytes()), Err(ConnectError::Unavailable)));
        });
        assert!(wire[0].starts_with("POST /v1/pair HTTP/1.1\r\n"));
        assert!(wire[0].ends_with(r#"{"provider":"telegram"}"#));
        for request in &wire[1..] {
            assert!(request.starts_with("POST /v1/claim HTTP/1.1\r\n"));
            assert!(request.ends_with(&format!(r#"{{"deviceKey":"{DEVICE}"}}"#)));
        }
        assert!(wire.iter().all(|request| !request.contains("Authorization:") && !request.contains("Origin:")));
    }

    #[test]
    fn responses_cannot_point_outside_the_provider_or_expose_a_different_kind_of_secret() {
        for url in [
            format!("https://t.me/MiloBot?start={DEVICE}"),
            format!("https://t.me.example.org/MiloBot?start={LINK_KEY}"),
            format!("https://t.me/MiloBot?start={LINK_KEY}&extra=1"),
            format!("{ORIGIN}/discord/start/{LINK_KEY}"),
        ] {
            let reply = format!(r#"{{"deviceKey":"{DEVICE}","url":"{url}"}}"#);
            assert!(paired(Destination::Telegram, &reply).is_none());
        }
        let discord_pair = format!(r#"{{"deviceKey":"{DEVICE}","url":"{ORIGIN}/discord/start/{LINK_KEY}"}}"#);
        assert_eq!(paired(Destination::Discord, &discord_pair).as_deref(), Some(discord_pair.as_str()));
        let discord_claim = r#"{"provider":"discord","label":"Discord channel · 1234567890","credential":"https://discord.com/api/webhooks/1234567891/synthetic_SYNTHETICsynthetic-01","chat":""}"#;
        assert_eq!(claimed(discord_claim).as_deref(), Some(discord_claim));
        for (credential, chat, label) in [(MANUAL, "42", "42"), (CAPABILITY, "-42", "-42"), (CAPABILITY, "42", "41")] {
            let reply = format!(
                r#"{{"provider":"telegram","label":"Telegram DM · {label}","credential":"{credential}","chat":"{chat}"}}"#,
            );
            assert!(claimed(&reply).is_none());
        }
    }

    #[test]
    fn a_saved_relay_capability_sends_only_to_the_relay_and_is_revoked_when_replaced_or_forgotten() {
        let replies = [(200, "{}"), (204, ""), (200, "{}"), (401, "")];
        let (_, wire) = with_replies(&replies, |origin| {
            let f = Fixture::folder();
            let mut alerts = Alerts::at(&f.root.join("config"), &f.root.join("state"));
            alerts.stand_in = Some(origin.to_owned());
            alerts.set_telegram(MANUAL, "-1001234567890").unwrap();
            alerts.enable().unwrap();
            assert!(alerts.set_hosted_telegram(MANUAL, "42").is_err());
            assert!(alerts.set_hosted_telegram(CAPABILITY, "-42").is_err());
            alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
            assert!(!alerts.settings().unwrap().enabled);
            assert!(!std::fs::read_to_string(alerts.file()).unwrap().contains(CAPABILITY));
            assert!(matches!(alerts.telegram_chats(CAPABILITY), Err(TelegramChatsError::InvalidToken)));
            assert!(alerts.message_request(Destination::Discord, CAPABILITY, "", "test", None).is_none());
            alerts.test().unwrap();
            alerts.set_telegram(MANUAL, "@synthetic_chat").unwrap();
            alerts.test().unwrap();
            alerts.set_telegram(CAPABILITY, "42").unwrap();
            alerts.forget(Destination::Telegram).unwrap();
            assert!(!alerts.summary().unwrap().saved(Destination::Telegram));
        });
        let bearer = format!("\r\nAuthorization: Bearer {CAPABILITY}\r\n");
        assert!(wire[0].starts_with("POST /v1/send HTTP/1.1\r\n") && wire[0].contains(&bearer));
        assert!(wire[0].ends_with(&format!("{{\"text\":{}}}", json(crate::alerts::message::TEST))));
        for revocation in [&wire[1], &wire[3]] {
            assert!(revocation.starts_with("DELETE /v1/binding HTTP/1.1\r\n") && revocation.contains(&bearer));
            assert!(revocation.ends_with("\r\n\r\n"));
        }
        assert!(wire[2].starts_with(&format!("POST /bot{MANUAL}/sendMessage HTTP/1.1\r\n")));
        assert!(!wire[2].contains(&bearer) && wire[2].contains("\"chat_id\":\"@synthetic_chat\""));
    }

    #[test]
    fn failed_revocation_keeps_the_key_and_forgetting_an_unused_connection_preserves_forwarding() {
        let replies = [(503, "{}"), (503, "{}"), (204, "")];
        let (_, wire) = with_replies(&replies, |origin| {
            const WEBHOOK: &str = "https://discord.com/api/webhooks/1234567891/synthetic_SYNTHETICsynthetic-01";
            let f = Fixture::folder();
            let mut alerts = Alerts::at(&f.root.join("config"), &f.root.join("state"));
            alerts.stand_in = Some(origin.to_owned());
            alerts.set_telegram(CAPABILITY, "42").unwrap();
            alerts.enable().unwrap();
            assert!(alerts.set_telegram(MANUAL, "43").unwrap_err().contains("Could not disconnect"));
            assert_eq!(alerts.settings().unwrap().telegram_chat, "42");
            assert!(alerts.forget(Destination::Telegram).unwrap_err().contains("Could not disconnect"));
            assert!(!alerts.settings().unwrap().enabled);
            assert_eq!(alerts.secret(Destination::Telegram).unwrap(), CAPABILITY);
            alerts.set_discord(WEBHOOK).unwrap();
            alerts.enable().unwrap();
            alerts.forget(Destination::Telegram).unwrap();
            let settings = alerts.settings().unwrap();
            assert!(settings.enabled && settings.destination == Some(Destination::Discord));
            assert!(!alerts.summary().unwrap().saved(Destination::Telegram));
        });
        assert!(wire.iter().all(|request| request.starts_with("DELETE /v1/binding HTTP/1.1\r\n")));
    }
}
