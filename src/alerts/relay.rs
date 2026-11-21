//! Pairing and Telegram forwarding through Milo's production or loopback relay.

use std::borrow::Cow;

use super::{Destination, send, valid_webhook_path};
use crate::json::Value;
use crate::rex::json;

const PRODUCTION: &str = "https://connect.usemilo.net";
const LOCAL: &str = "http://127.0.0.1:6120";
pub(crate) const REQUEST_LIMIT: usize = 1024;
const RESPONSE_LIMIT: u64 = 4096;
const KEY_LEN: usize = 43;
const LOCAL_PREFIX: &str = "milorelay_";
const PRODUCTION_PREFIX: &str = "milorelay_prod_";
const FINISH_LEN: usize = 8;

pub(super) enum Origin {
    Production,
    Local,
    #[cfg(test)]
    Test(String),
}

impl Origin {
    pub(super) fn configured() -> Option<Self> {
        match std::env::var("MILO_RELAY_ORIGIN") {
            Ok(origin) => Self::parse(&origin),
            Err(std::env::VarError::NotPresent) => Some(Self::Production),
            Err(std::env::VarError::NotUnicode(_)) => None,
        }
    }

    fn parse(origin: &str) -> Option<Self> {
        match origin {
            PRODUCTION => Some(Self::Production),
            LOCAL => Some(Self::Local),
            _ => None,
        }
    }

    fn production(&self) -> bool {
        matches!(self, Self::Production)
    }

    fn url(&self) -> &str {
        match self {
            Self::Production => PRODUCTION,
            Self::Local => LOCAL,
            #[cfg(test)]
            Self::Test(url) => url,
        }
    }

    fn prefix(&self) -> &str {
        if self.production() { PRODUCTION_PREFIX } else { LOCAL_PREFIX }
    }

    pub(super) fn accepts(&self, capability: &str) -> bool {
        capability.strip_prefix(self.prefix()).is_some_and(valid_key)
    }

    pub(super) fn send_request(&self, capability: &str, text: &str) -> Option<send::Request> {
        self.accepts(capability).then(|| {
            let address = format!("{}/v1/send", self.url());
            let body = format!("{{\"text\":{}}}", json(text));
            send::relay_request(&address, "POST", Some(&body), Some(capability))
        })
    }

    /// Production confirms absence only from the same, readable issuer ledger.
    pub(super) fn revoke(&self, capability: &str) -> bool {
        if !self.accepts(capability) {
            return false;
        }
        let address = format!("{}/v1/binding", self.url());
        let request = send::relay_request(&address, "DELETE", None, Some(capability));
        match send::response(&request, RESPONSE_LIMIT, None) {
            Some((body, 204)) => body.is_empty(),
            Some((_, 401)) => !self.production(),
            _ => false,
        }
    }
}

pub(super) fn valid_capability(secret: &str) -> bool {
    Origin::Production.accepts(secret) || Origin::Local.accepts(secret)
}

fn valid_key(key: &str) -> bool {
    key.len() == KEY_LEN && key.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

pub(super) fn valid_chat(chat: &str) -> bool {
    !chat.starts_with('0') && chat.bytes().all(|byte| byte.is_ascii_digit()) && chat.parse::<i64>().is_ok()
}

/// Hosted Discord claims only return a modern, canonical webhook.
pub(super) fn hosted_webhook(credential: &str) -> Option<&str> {
    let path = credential.strip_prefix("https://discord.com/api/webhooks/")?;
    let (id, _) = path.split_once('/')?;
    (valid_webhook_path(path) && id.len() <= 20 && !id.starts_with('0') && id.parse::<u64>().is_ok()).then_some(path)
}

fn valid_finish(code: &str) -> bool {
    code.len() == FINISH_LEN && code.bytes().all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
}

pub(crate) enum Connected {
    Ready(String),
    Pending,
    Claimed(Claim),
}

pub(crate) enum ConnectError {
    Invalid,
    Proof,
    Expired,
    Busy,
    Disabled,
    Unavailable,
}

pub(crate) struct ConnectionRequest<'a> {
    origin: Origin,
    operation: Pairing<'a>,
}

impl<'a> ConnectionRequest<'a> {
    pub(crate) fn parse(body: &'a [u8]) -> Result<Self, ConnectError> {
        let origin = Origin::configured().ok_or(ConnectError::Unavailable)?;
        let operation = Pairing::parse(&origin, body).ok_or(ConnectError::Invalid)?;
        Ok(Self { origin, operation })
    }

    pub(crate) fn save(&self) -> Option<(u64, &str)> {
        match &self.operation {
            Pairing::Save { version, key, .. } => Some((*version, key)),
            _ => None,
        }
    }

    pub(crate) fn run(&self) -> Result<Connected, ConnectError> {
        self.operation.run(&self.origin)
    }
    /// The caller holds the alert settings lock and has confirmed this credential was not saved.
    pub(crate) fn compensate(&self, claim: &Claim) -> bool {
        match claim.provider {
            Destination::Telegram => self.origin.revoke(&claim.credential),
            Destination::Discord => {
                let request = send::relay_request(&claim.credential, "DELETE", None, None);
                matches!(send::response(&request, RESPONSE_LIMIT, None), Some((_, 204 | 404)))
            }
        }
    }
}

pub(crate) struct Claim {
    pub(crate) provider: Destination,
    pub(crate) credential: String,
    pub(crate) chat: String,
}

enum Pairing<'a> {
    Start(Destination),
    Preview { provider: Destination, key: Cow<'a, str>, finish: Option<Cow<'a, str>> },
    Save {
        provider: Destination,
        key: Cow<'a, str>,
        finish: Option<Cow<'a, str>>,
        label: Cow<'a, str>,
        version: u64,
    },
}

#[cfg(test)]
fn connect_at(origin: &Origin, body: &[u8]) -> Result<Connected, ConnectError> {
    Pairing::parse(origin, body).ok_or(ConnectError::Invalid)?.run(origin)
}

impl<'a> Pairing<'a> {
    fn parse(origin: &Origin, body: &'a [u8]) -> Option<Self> {
        if body.len() > REQUEST_LIMIT {
            return None;
        }
        let [action, provider, key, finish, label, version] = fields(
            std::str::from_utf8(body).ok()?,
            ["action", "provider", "deviceKey", "finishCode", "label", "version"],
        )?;
        let provider = Destination::parse(&provider?.string(8)?)?;
        let action = action?.string(7)?;
        if action == "start" && key.is_none() && finish.is_none() && label.is_none() && version.is_none() {
            return Some(Self::Start(provider));
        }
        let key = key?.string(KEY_LEN)?;
        let finish = match (origin.production(), finish) {
            (true, Some(value)) => Some(value.string(FINISH_LEN)?),
            (false, None) => None,
            _ => return None,
        };
        if !valid_key(&key) || !finish.as_deref().is_none_or(valid_finish) {
            return None;
        }
        match (action.as_ref(), label, version) {
            ("preview", None, None) => Some(Self::Preview { provider, key, finish }),
            ("save", Some(label), Some(version)) => {
                let label = label.string(40)?;
                label_chat(provider, &label)?;
                Some(Self::Save { provider, key, finish, label, version: version.unsigned()? })
            }
            _ => None,
        }
    }

    fn run(&self, origin: &Origin) -> Result<Connected, ConnectError> {
        let (reply, status) = send::response(&self.request(origin), RESPONSE_LIMIT, None).ok_or(ConnectError::Unavailable)?;
        self.reply(origin, status, &reply)
    }

    fn request(&self, origin: &Origin) -> send::Request {
        let (path, body) = match self {
            Self::Start(provider) => ("pair", format!("{{\"provider\":\"{}\"}}", provider.name())),
            Self::Preview { key, finish, .. } | Self::Save { key, finish, .. } => {
                let path = if matches!(self, Self::Save { .. }) { "claim" } else { "preview" };
                let body = match finish {
                    Some(finish) => format!("{{\"deviceKey\":\"{key}\",\"finishCode\":\"{finish}\"}}"),
                    None => format!("{{\"deviceKey\":\"{key}\"}}"),
                };
                (path, body)
            }
        };
        send::relay_request(&format!("{}/v1/{path}", origin.url()), "POST", Some(&body), None)
    }

    fn reply(&self, origin: &Origin, status: u16, reply: &str) -> Result<Connected, ConnectError> {
        use ConnectError::{Busy, Disabled, Expired, Proof, Unavailable};
        match (self, status) {
            (Self::Start(provider), 200) => paired(origin, *provider, reply).map(Connected::Ready).ok_or(Unavailable),
            (Self::Preview { provider, .. }, 200) => {
                let [name, label, chat] = fields(reply, ["provider", "label", "chat"]).ok_or(Unavailable)?;
                let review = Review::parse(*provider, name, label, chat).ok_or(Unavailable)?;
                Ok(Connected::Ready(review.json()))
            }
            (Self::Save { provider, label, .. }, 200) => {
                let (review, credential) = claimed(origin, *provider, reply).ok_or(Unavailable)?;
                if &review.label != label {
                    return Err(Unavailable);
                }
                Ok(Connected::Claimed(Claim {
                    provider: *provider,
                    credential: credential.into_owned(),
                    chat: review.chat.into_owned(),
                }))
            }
            (Self::Preview { .. } | Self::Save { .. }, 202) => match fields(reply, ["pending"]) {
                Some([Some(value)]) if value.boolean() == Some(true) => Ok(Connected::Pending),
                _ => Err(Unavailable),
            },
            (Self::Preview { finish: Some(_), .. } | Self::Save { finish: Some(_), .. }, 400) => Err(Proof),
            (Self::Preview { .. } | Self::Save { .. }, 404) => Err(Expired),
            (_, 429) => Err(Busy),
            (_, 503) => Err(Disabled),
            _ => Err(Unavailable),
        }
    }
}

/// Validate and reconstruct success replies. Don't forward arbitrary upstream fields or error text.
fn paired(origin: &Origin, provider: Destination, reply: &str) -> Option<String> {
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
        Destination::Discord => {
            let path = if origin.production() {
                url.strip_prefix(PRODUCTION)?
            } else {
                url.strip_prefix(LOCAL).or_else(|| url.strip_prefix("http://localhost:6120"))?
            };
            path.strip_prefix("/discord/start/")?
        }
    };
    // The browser link is not authority to claim and obtain the credential.
    (valid_key(&key) && valid_key(public_key) && public_key != key).then(|| {
        let mode = if origin.production() { "production" } else { "local" };
        format!("{{\"deviceKey\":\"{key}\",\"url\":\"{url}\",\"mode\":\"{mode}\"}}")
    })
}

fn label_chat(provider: Destination, label: &str) -> Option<&str> {
    match provider {
        Destination::Telegram => {
            let chat = label.strip_prefix("Telegram DM · ")?;
            valid_chat(chat).then_some(chat)
        }
        Destination::Discord => {
            let id = label.strip_prefix("Discord channel · ")?;
            (!id.starts_with('0') && id.bytes().all(|byte| byte.is_ascii_digit()) && id.parse::<u64>().is_ok())
                .then_some("")
        }
    }
}

struct Review<'a> {
    provider: Destination,
    label: Cow<'a, str>,
    chat: Cow<'a, str>,
}

impl<'a> Review<'a> {
    fn parse(
        provider: Destination,
        name: Option<Value<'a>>,
        label: Option<Value<'a>>,
        chat: Option<Value<'a>>,
    ) -> Option<Self> {
        if !name?.is(provider.name()) {
            return None;
        }
        let (label, chat) = (label?.string(40)?, chat?.string(19)?);
        (label_chat(provider, &label)? == chat.as_ref()).then_some(Self { provider, label, chat })
    }

    fn json(&self) -> String {
        let Self { provider, label, chat } = self;
        format!("{{\"provider\":\"{}\",\"label\":\"{label}\",\"chat\":\"{chat}\"}}", provider.name())
    }
}

fn claimed<'a>(origin: &Origin, provider: Destination, reply: &'a str) -> Option<(Review<'a>, Cow<'a, str>)> {
    let [name, label, credential, chat] = fields(reply, ["provider", "label", "credential", "chat"])?;
    let review = Review::parse(provider, name, label, chat)?;
    let credential = credential?.string(256)?;
    let valid = match provider {
        Destination::Telegram => origin.accepts(&credential),
        Destination::Discord => hosted_webhook(&credential).is_some(),
    };
    valid.then_some((review, credential))
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
    const PROD_CAPABILITY: &str = concat!("milorelay_prod_", "0123456789", "abcdefghij", "ABCDEFGHIJ", "9876543210", "_-x");
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
                        if *status != 0 {
                            write!(
                                socket,
                                "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len(),
                            )
                            .unwrap();
                        }
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
            r#"{"action":"preview","provider":"telegram","deviceKey":"anything"}"#,
            r#"{"action":"preview","provider":"telegram","deviceKey":true}"#,
        ] {
            assert!(Pairing::parse(&Origin::Local, body.as_bytes()).is_none(), "{body}");
        }
        let large = format!(r#"{{"action":"start","provider":"telegram"}}{}"#, " ".repeat(REQUEST_LIMIT));
        assert!(Pairing::parse(&Origin::Local, large.as_bytes()).is_none());
    }

    #[test]
    fn production_preview_requires_finish_proof_and_only_places_it_in_the_https_request_body() {
        let origin = Origin::Production;
        let claim = format!(r#"{{"action":"preview","provider":"discord","deviceKey":"{DEVICE}"}}"#);
        assert!(Pairing::parse(&origin, claim.as_bytes()).is_none());
        for finish in ["123", "ABCDE1234", "abcd1234", "ABCX1234"] {
            let body = format!(
                r#"{{"action":"preview","provider":"discord","deviceKey":"{DEVICE}","finishCode":"{finish}"}}"#,
            );
            assert!(Pairing::parse(&origin, body.as_bytes()).is_none());
        }
        let body = format!(
            r#"{{"action":"preview","provider":"discord","deviceKey":"{DEVICE}","finishCode":"ABCD1234"}}"#,
        );
        let parse = || Pairing::parse(&origin, body.as_bytes()).unwrap();
        assert!(Pairing::parse(&Origin::Local, body.as_bytes()).is_none());
        let options = parse().request(&origin).options;
        assert!(options.starts_with("url = \"https://connect.usemilo.net/v1/preview\"\n"));
        assert!(options.contains("proto = \"=https\"\n") && options.contains("tlsv1.2\n"));
        assert!(options.contains("noproxy = \"*\"\n") && options.contains("max-redirs = 0\n"));
        let data = format!(r#"data-raw = "{{\"deviceKey\":\"{DEVICE}\",\"finishCode\":\"ABCD1234\"}}""#);
        assert!(options.contains(&data));
        assert!(!options.contains("Authorization:") && !options.contains("provider"));
        assert!(matches!(parse().reply(&origin, 400, "private detail"), Err(ConnectError::Proof)));
        assert!(matches!(parse().reply(&origin, 404, "private detail"), Err(ConnectError::Expired)));
        assert!(matches!(
            parse().reply(&origin, 202, r#"{"pending":true,"detail":"private"}"#),
            Err(ConnectError::Unavailable)
        ));

        let review = r#"{"provider":"discord","label":"Discord channel · 12345","chat":""}"#;
        let claim = r#"{"provider":"discord","label":"Discord channel · 12345","chat":"","credential":"https://discord.com/api/webhooks/1234567891/synthetic_SYNTHETICsynthetic-01"}"#;
        assert!(matches!(parse().reply(&origin, 200, review), Ok(Connected::Ready(metadata)) if metadata == review));
        assert!(matches!(parse().reply(&origin, 200, claim), Err(ConnectError::Unavailable)));

        let body = format!(
            r#"{{"action":"save","provider":"discord","deviceKey":"{DEVICE}","finishCode":"ABCD1234","label":"Discord channel · 12345","version":9}}"#,
        );
        let finalize = Pairing::parse(&origin, body.as_bytes()).unwrap();
        let options = finalize.request(&origin).options;
        assert!(options.starts_with("url = \"https://connect.usemilo.net/v1/claim\"\n") && options.contains(&data));
        assert!(!options.contains("label") && !options.contains("version"));
        assert!(matches!(finalize.reply(&origin, 200, review), Err(ConnectError::Unavailable)));
        let changed = claim.replace(" · 12345", " · 12346");
        assert!(matches!(finalize.reply(&origin, 200, &changed), Err(ConnectError::Unavailable)));
        assert!(matches!(
            finalize.reply(&origin, 200, claim),
            Ok(Connected::Claimed(claim)) if claim.provider == Destination::Discord && claim.chat.is_empty()
        ));
    }

    #[test]
    fn only_the_two_named_origins_are_supported_and_capabilities_cannot_cross_them() {
        assert!(matches!(Origin::parse("https://connect.usemilo.net"), Some(Origin::Production)));
        assert!(matches!(Origin::parse("http://127.0.0.1:6120"), Some(Origin::Local)));
        for origin in [
            "",
            "http://connect.usemilo.net",
            "https://connect.usemilo.net/",
            "https://connect.usemilo.net:443",
            "https://connect.usemilo.net.example.invalid",
            "http://127.0.0.1:6121",
        ] {
            assert!(Origin::parse(origin).is_none(), "{origin}");
        }
        assert!(valid_capability(CAPABILITY) && valid_capability(PROD_CAPABILITY));
        assert!(Origin::Production.send_request(CAPABILITY, "text").is_none());
        assert!(Origin::Local.send_request(PROD_CAPABILITY, "text").is_none());
        assert!(!Origin::Production.revoke(CAPABILITY));
        assert!(!Origin::Local.revoke(PROD_CAPABILITY));
        let f = Fixture::folder();
        let mut alerts = Alerts::at(&f.root.join("config"), &f.root.join("state"));
        alerts.stand_in = Some("http://127.0.0.1:1".into());
        alerts.relay = Some(Origin::Production);
        let sealed = format!("{PROD_CAPABILITY}:42");
        let options = alerts.message_request(Destination::Telegram, &sealed, "42", "text", None).unwrap().options;
        assert!(options.starts_with("url = \"https://connect.usemilo.net/v1/send\"\n"));
        assert!(options.contains(&format!("Authorization: Bearer {PROD_CAPABILITY}")));

        alerts.relay = Some(Origin::Local);
        assert!(alerts.set_hosted_telegram(PROD_CAPABILITY, "42").is_err());
        alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
        alerts.relay = Some(Origin::Production);
        let sealed = format!("{CAPABILITY}:42");
        assert!(alerts.message_request(Destination::Telegram, &sealed, "42", "text", None).is_none());
        assert!(alerts.test_destination(Destination::Telegram).unwrap_err().contains("different relay environment"));
        assert!(alerts.forget(Destination::Telegram).unwrap_err().contains("Could not disconnect"));
        assert_eq!(alerts.secret(Destination::Telegram).unwrap(), sealed);
    }

    #[test]
    fn pairing_reconstructs_bounded_replies_and_never_forwards_browser_authority() {
        let start = br#"{"action":"start","provider":"telegram"}"#;
        let preview = format!(r#"{{"action":"preview","provider":"telegram","deviceKey":"{DEVICE}"}}"#);
        let save = format!(
            r#"{{"action":"save","provider":"telegram","deviceKey":"{DEVICE}","label":"Telegram DM · 42","version":1}}"#,
        );
        let pair = format!(r#"{{"deviceKey":"{DEVICE}","url":"https://t.me/MiloBot?start={LINK_KEY}"}}"#);
        let expected = format!(
            r#"{{"deviceKey":"{DEVICE}","url":"https://t.me/MiloBot?start={LINK_KEY}","mode":"local"}}"#,
        );
        let metadata = r#"{"provider":"telegram","label":"Telegram DM · 42","chat":"42"}"#;
        let ready = format!(
            r#"{{"provider":"telegram","label":"Telegram DM · 42","credential":"{CAPABILITY}","chat":"42"}}"#,
        );
        let huge = "x".repeat(usize::try_from(RESPONSE_LIMIT).unwrap() + 1);
        let replies = [
            (200, pair.as_str()),
            (202, r#"{"pending":true}"#),
            (200, ready.as_str()),
            (200, metadata),
            (200, ready.as_str()),
            (404, MANUAL),
            (200, huge.as_str()),
        ];
        let (_, wire) = with_replies(&replies, |origin| {
            let origin = &Origin::Test(origin.to_owned());
            assert!(matches!(connect_at(origin, start), Ok(Connected::Ready(body)) if body == expected));
            assert!(matches!(connect_at(origin, preview.as_bytes()), Ok(Connected::Pending)));
            assert!(matches!(connect_at(origin, preview.as_bytes()), Err(ConnectError::Unavailable)));
            assert!(matches!(connect_at(origin, preview.as_bytes()), Ok(Connected::Ready(body)) if body == metadata));
            assert!(matches!(
                connect_at(origin, save.as_bytes()),
                Ok(Connected::Claimed(claim)) if claim.credential == CAPABILITY && claim.chat == "42"
            ));
            assert!(matches!(connect_at(origin, save.as_bytes()), Err(ConnectError::Expired)));
            assert!(matches!(connect_at(origin, save.as_bytes()), Err(ConnectError::Unavailable)));
        });
        assert!(wire[0].starts_with("POST /v1/pair HTTP/1.1\r\n"));
        assert!(wire[0].ends_with(r#"{"provider":"telegram"}"#));
        for (index, request) in wire.iter().enumerate().skip(1) {
            let path = if index <= 3 { "preview" } else { "claim" };
            assert!(request.starts_with(&format!("POST /v1/{path} HTTP/1.1\r\n")));
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
            format!("https://t.me:443/MiloBot?start={LINK_KEY}"),
            format!("{LOCAL}/discord/start/{LINK_KEY}"),
        ] {
            let reply = format!(r#"{{"deviceKey":"{DEVICE}","url":"{url}"}}"#);
            assert!(paired(&Origin::Production, Destination::Telegram, &reply).is_none());
        }
        for (url_origin, mode) in [(PRODUCTION, "production"), (LOCAL, "local"), ("http://localhost:6120", "local")] {
            let url = format!("{url_origin}/discord/start/{LINK_KEY}");
            let reply = format!(r#"{{"deviceKey":"{DEVICE}","url":"{url}"}}"#);
            let expected = format!(r#"{{"deviceKey":"{DEVICE}","url":"{url}","mode":"{mode}"}}"#);
            let (matches, opposite) = if mode == "production" {
                (Origin::Production, Origin::Local)
            } else {
                (Origin::Local, Origin::Production)
            };
            assert_eq!(paired(&matches, Destination::Discord, &reply).as_deref(), Some(expected.as_str()));
            assert!(paired(&opposite, Destination::Discord, &reply).is_none());
        }
        let discord_claim = r#"{"provider":"discord","label":"Discord channel · 1234567890","credential":"https://discord.com/api/webhooks/1234567891/synthetic_SYNTHETICsynthetic-01","chat":""}"#;
        assert!(claimed(&Origin::Production, Destination::Discord, discord_claim).is_some());
        assert!(claimed(&Origin::Production, Destination::Telegram, discord_claim).is_none());
        let old_host = discord_claim.replace("discord.com", "discordapp.com");
        assert!(claimed(&Origin::Production, Destination::Discord, &old_host).is_none());
        for (credential, chat, label) in [
            (MANUAL, "42", "42"),
            (CAPABILITY, "-42", "-42"),
            (CAPABILITY, "42", "41"),
            (PROD_CAPABILITY, "42", "42"),
        ] {
            let reply = format!(
                r#"{{"provider":"telegram","label":"Telegram DM · {label}","credential":"{credential}","chat":"{chat}"}}"#,
            );
            assert!(claimed(&Origin::Local, Destination::Telegram, &reply).is_none());
        }
        let production = format!(
            r#"{{"provider":"telegram","label":"Telegram DM · 42","credential":"{PROD_CAPABILITY}","chat":"42"}}"#,
        );
        let (_, credential) = claimed(&Origin::Production, Destination::Telegram, &production).unwrap();
        assert_eq!(credential, PROD_CAPABILITY);
        let local = production.replace(PROD_CAPABILITY, CAPABILITY);
        assert!(claimed(&Origin::Production, Destination::Telegram, &local).is_none());
        let (_, credential) = claimed(&Origin::Local, Destination::Telegram, &local).unwrap();
        assert_eq!(credential, CAPABILITY);
    }

    #[test]
    fn the_relay_s_rate_limit_says_how_long_to_wait() {
        let throttled = r#"{"error":"Too many requests; try again later.","retry_after":35}"#;
        let (outcome, _) = with_replies(&[(429, throttled)], |origin| {
            let request = Origin::Test(origin.to_owned()).send_request(CAPABILITY, "text").unwrap();
            send::post(&request, None)
        });
        assert_eq!(outcome, send::Outcome::Wait(Duration::from_secs(35)));
    }

    #[test]
    fn a_rejected_relay_message_and_one_with_unknown_delivery_are_distinct() {
        let replies = [(422, "{}"), (502, "{}"), (200, "<html>OK</html>"), (0, "")];
        let (outcomes, _) = with_replies(&replies, |origin| {
            let request = Origin::Test(origin.to_owned()).send_request(CAPABILITY, "text").unwrap();
            replies.map(|_| send::post(&request, None))
        });
        assert_eq!(
            outcomes,
            [
                send::Outcome::Refused(422),
                send::Outcome::Uncertain(Some(502)),
                send::Outcome::Uncertain(Some(200)),
                send::Outcome::Uncertain(None),
            ],
        );
    }

    #[test]
    fn a_saved_relay_capability_sends_only_to_the_relay_and_is_revoked_when_replaced_or_forgotten() {
        let replies = [(204, ""), (204, ""), (200, "{}"), (401, "")];
        let (_, wire) = with_replies(&replies, |origin| {
            let f = Fixture::folder();
            let mut alerts = Alerts::at(&f.root.join("config"), &f.root.join("state"));
            alerts.stand_in = Some(origin.to_owned());
            alerts.relay = Some(Origin::Test(origin.to_owned()));
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
            alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
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
            alerts.relay = Some(Origin::Test(origin.to_owned()));
            alerts.set_hosted_telegram(CAPABILITY, "42").unwrap();
            alerts.enable().unwrap();
            assert!(alerts.set_telegram(MANUAL, "43").unwrap_err().contains("Could not disconnect"));
            assert_eq!(alerts.settings().unwrap().telegram_chat, "42");
            assert!(alerts.forget(Destination::Telegram).unwrap_err().contains("Could not disconnect"));
            assert!(!alerts.settings().unwrap().enabled);
            assert_eq!(alerts.secret(Destination::Telegram).unwrap(), format!("{CAPABILITY}:42"));
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
