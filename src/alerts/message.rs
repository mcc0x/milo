//! Words an alert and builds the request body each destination expects.
//!
//! Everything a program gave Rex is untrusted: its short name as much as its
//! text. Each is sent as literal text inside the destination's code mark-up,
//! where neither Telegram nor Discord makes a link, a mention or a command of
//! it. The text is also one line, cut short, and has every `@` broken; and
//! Discord is told outright to notify no user, role or group.

use super::event::{Alert, Waiting};
use super::{Content, Destination, Kind, Settings};
use crate::rex::json;

/// The message `milo alerts test` sends. No Rex alert is involved in it.
pub(crate) const TEST: &str =
    "Milo test: Rex alert mirroring can reach this destination. This is not a Rex alert, and nothing else was sent.";
/// How much of a program's own text is sent, in characters.
const TEXT_LIMIT: usize = 300;
const MILO_BLUE: u32 = 0x4582f9;
const MILO_ICON: &str = "https://usemilo.net/apple-touch-icon.png";

/// UTC send time for Discord's localized embed footer, fixed across delivery retries.
fn discord_timestamp() -> String {
    let now = libc::time_t::try_from(crate::when::now()).unwrap_or(0);
    let mut utc = std::mem::MaybeUninit::uninit();
    // SAFETY: both pointers are valid for this call, which writes nothing outside the tm.
    if unsafe { libc::gmtime_r(&now, utc.as_mut_ptr()) }.is_null() {
        return String::new();
    }
    // SAFETY: gmtime_r succeeded, which initializes the entire tm.
    let utc = unsafe { utc.assume_init() };
    format!(
        ",\"timestamp\":\"{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z\"",
        utc.tm_year + 1900,
        utc.tm_mon + 1,
        utc.tm_mday,
        utc.tm_hour,
        utc.tm_min,
        utc.tm_sec
    )
}

/// The message for one alert, in the destination's own mark-up. `unsent`
/// counts alerts dropped since the last one sent.
pub(crate) fn text(alert: &Alert, content: Content, unsent: u32, destination: Destination) -> String {
    let (subject, action) = match (alert.kind, alert.waiting) {
        (Kind::Blocked, Some(Waiting::Permission)) => (alert.app.as_deref(), "is waiting for permission"),
        (Kind::Blocked, Some(Waiting::Question)) => (alert.app.as_deref(), "is waiting for an answer"),
        (Kind::Blocked, Some(Waiting::Auth)) => (alert.app.as_deref(), "is waiting for you to sign in"),
        (Kind::Blocked, None) => (alert.app.as_deref(), "is waiting for you"),
        (Kind::Error, _) => (alert.app.as_deref(), "reported an error"),
        (Kind::Done, _) => (alert.app.as_deref(), "finished"),
        (Kind::Notification, _) => (None, "sent a notification"),
        (Kind::Bell, _) => (None, "rang its bell"),
    };
    let fallback = if alert.kind == Kind::Bell { "a terminal" } else { "a program" };
    let subject = subject.map(|name| literal(name, destination));
    let subject = subject.as_deref().unwrap_or(fallback);
    let mut message = match destination {
        Destination::Telegram => format!("Rex: {subject} {action}"),
        Destination::Discord => format!("{subject} **{action}**"),
    };
    if content == Content::Text {
        let said = plain(&alert.text);
        if !said.is_empty() {
            message.push('\n');
            message.push_str(&literal(&said, destination));
        }
    }
    if unsent > 0 {
        let were = if unsent == 1 { "alert was" } else { "alerts were" };
        message.push_str(&format!("\n({unsent} earlier {were} not sent.)"));
    }
    message
}

/// Text a program supplied, marked as code so that the destination shows it
/// as it is. Telegram reads the message as HTML, where code holds no link,
/// mention or command; Discord reads Markdown, where a code span holds none
/// either and ends only at a backtick.
fn literal(text: &str, destination: Destination) -> String {
    match destination {
        Destination::Telegram => {
            let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            format!("<code>{escaped}</code>")
        }
        Destination::Discord => format!("`{}`", text.replace('`', "'")),
    }
}

/// Characters that show nothing, or reorder, hide or pad what follows.
/// One of them is what breaks `@` below, so none may arrive already in place.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{ad}' | '\u{61c}' | '\u{115f}' | '\u{1160}' | '\u{180e}' | '\u{2800}' | '\u{3164}' | '\u{ffa0}' | '\u{feff}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{e0000}'..='\u{e01ef}'
        )
}

/// A program's text as one short line that mentions nobody.
fn plain(text: &str) -> String {
    let mut line = String::new();
    for (count, c) in text.chars().map(|c| if hidden(c) { ' ' } else { c }).enumerate() {
        if count == TEXT_LIMIT {
            line.push('…');
            break;
        }
        line.push(c);
        if c == '@' {
            line.push('\u{200b}');
        }
    }
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The JSON a destination's send request takes.
pub(crate) fn body(destination: Destination, chat: &str, text: &str, kind: Option<Kind>) -> String {
    match destination {
        // A public channel is named as a string; any other chat by its number.
        Destination::Telegram => {
            let chat = if chat.starts_with('@') { json(chat) } else { chat.to_owned() };
            format!(
                "{{\"chat_id\":{chat},\"text\":{},\"parse_mode\":\"HTML\",\"link_preview_options\":{{\"is_disabled\":true}}}}",
                json(text)
            )
        }
        Destination::Discord => {
            let test = kind.is_none() && text == TEST;
            let (title, color) = match kind {
                Some(Kind::Blocked) => ("Needs attention", 0xe5a53b),
                Some(Kind::Error) => ("Program error", 0xd95363),
                Some(Kind::Done) => ("Finished", 0x369b73),
                Some(Kind::Notification) => ("Notification", MILO_BLUE),
                Some(Kind::Bell) => ("Terminal bell", MILO_BLUE),
                None if test => ("Connection test", MILO_BLUE),
                None => ("Forwarding status", MILO_BLUE),
            };
            // Discord puts the icon to the left of the author line, not the title.
            let description = if test { "**Connected.** Milo can send Rex alerts to this channel." } else { text };
            format!(
                "{{\"embeds\":[{{\"author\":{{\"name\":\"milo - {title}\",\"icon_url\":\"{MILO_ICON}\"}},\"color\":{color},\"description\":{}{}}}],\"allowed_mentions\":{{\"parse\":[]}}}}",
                json(description),
                discord_timestamp()
            )
        }
    }
}

/// What is sent for each alert with this choice of content.
pub(crate) fn sends(content: Content) -> &'static str {
    match content {
        Content::Metadata => {
            "Sends: the kind of alert and the program's short name, such as \"Rex: claude-code is waiting for \
             permission\". No text from the program, and no session, tab, folder or file names."
        }
        Content::Text => {
            "Sends: the kind of alert, the program's short name, and up to 300 characters of the text the program \
             gave Rex for it, which can name files or quote a prompt. No session, tab or folder names."
        }
    }
}

/// What leaves this machine with the current choices, in words and by example.
pub(crate) fn disclosure(settings: &Settings) -> String {
    let kinds: Vec<_> = Kind::ALL.into_iter().filter(|&kind| settings.mirrors(kind)).collect();
    if kinds.is_empty() {
        return "Mirrors: nothing. Choose kinds with milo alerts events, such as blocked,error,done.".into();
    }
    let meanings: Vec<_> = kinds.iter().map(|kind| format!("{} ({})", kind.name(), kind.meaning())).collect();
    format!("Mirrors: {}.\n{}", meanings.join(", "), sends(settings.content))
}
