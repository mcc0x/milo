//! Mirrors Rex alerts to the Telegram and Discord destinations the user chose.
//!
//! An *alert* is something Rex itself draws attention to: a program that says
//! through its terminal that it is blocked, failed or finished, one that asks
//! for a desktop notification, or one that rings the bell. Milo hears these on
//! Rex's event stream, beside Rex, so Rex's own notifications are untouched.
//! Mirroring is off until the user turns it on, and sends the kind of alert
//! and the program's short name only, unless the user asks for its text too.

mod cli;
mod consent;
mod event;
mod forward;
mod message;
pub(crate) mod relay;
mod secret;
mod send;
mod telegram;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) use cli::{HELP, command};
pub(crate) use consent::{Consent, ConsentFor, ConsentToken};
pub(crate) use event::Kind;
pub(crate) use message::disclosure;

use crate::setup::lock;
use crate::{paths, preferences};

const SETTINGS_LIMIT: u64 = 1 << 12;
/// Named by tests to send to a stand-in on this machine. A release build
/// does not read it: nothing changes where an installed Milo sends a secret.
const TEST_ENDPOINT: &str = "MILO_ALERTS_TEST_ENDPOINT";

pub(crate) enum TelegramChatsError {
    InvalidToken,
    Unavailable,
}

const NO_DESTINATION: &str =
    "Choose where alerts go first: milo alerts telegram CHAT, or milo alerts discord. Nothing was turned on.";
const UNREADABLE: &str = "Could not read Milo's alert settings (alerts.conf). They were left alone.";
const UNSAVED: &str = "Could not save Milo's alert settings.";
const REPAIR_TELEGRAM: &str =
    "The saved Telegram connection does not match a verified chat. Connect Telegram again in Milo.";

/// A place alerts can be mirrored to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Destination {
    Telegram,
    Discord,
}

impl Destination {
    pub(crate) const ALL: [Self; 2] = [Self::Telegram, Self::Discord];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Discord => "discord",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Telegram => "Telegram",
            Self::Discord => "Discord",
        }
    }

    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|destination| destination.name() == name)
    }
}

/// How much of an alert leaves this machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Content {
    /// The kind of alert and the program's short name.
    #[default]
    Metadata,
    /// Also the line of text the program gave Rex for the alert.
    Text,
}

impl Content {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Text => "text",
        }
    }

    pub(crate) fn parse(name: &str) -> Option<Self> {
        [Self::Metadata, Self::Text].into_iter().find(|content| content.name() == name)
    }
}

/// What the user chose. Secrets are kept apart, in `secret::Store`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) enabled: bool,
    pub(crate) destination: Option<Destination>,
    /// Explicit forwarding choices; a legacy file follows `destination` instead.
    targets: Option<[bool; 2]>,
    /// One choice for each kind, in `Kind::ALL` order.
    pub(crate) kinds: [bool; 5],
    pub(crate) content: Content,
    /// A chat's number or a public channel's `@name`. Not a secret.
    pub(crate) telegram_chat: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            destination: None,
            targets: None,
            kinds: [true, true, true, false, false],
            content: Content::Metadata,
            telegram_chat: String::new(),
        }
    }
}

impl Settings {
    fn parse(text: &str) -> io::Result<Self> {
        fn invalid(key: &str) -> io::Error {
            io::Error::new(io::ErrorKind::InvalidData, format!("invalid alert setting {key}"))
        }
        let mut settings = Self::default();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let (key, value) = line.split_once('=').ok_or_else(|| invalid("(expected key = value)"))?;
            let (key, value) = (key.trim(), value.trim());
            match key {
                "enabled" => settings.enabled = value.parse().map_err(|_| invalid(key))?,
                "destination" => settings.destination = Some(Destination::parse(value).ok_or_else(|| invalid(key))?),
                "targets" => {
                    let mut targets = [false; 2];
                    if !value.is_empty() {
                        for name in value.split(',') {
                            let destination = Destination::parse(name.trim()).ok_or_else(|| invalid(key))?;
                            targets[destination as usize] = true;
                        }
                    }
                    settings.targets = Some(targets);
                }
                "events" => settings.kinds = parse_kinds(value).ok_or_else(|| invalid(key))?,
                "content" => settings.content = Content::parse(value).ok_or_else(|| invalid(key))?,
                "telegram_chat" if valid_chat(value) => value.clone_into(&mut settings.telegram_chat),
                "telegram_chat" => return Err(invalid(key)),
                _ => {}
            }
        }
        Ok(settings)
    }

    fn render(&self) -> String {
        let mut text = format!("enabled = {}\n", self.enabled);
        if let Some(destination) = self.destination {
            let _ = writeln!(text, "destination = {}", destination.name());
        }
        if let Some(targets) = self.targets {
            text.push_str("targets = ");
            let mut names = Destination::ALL.into_iter().filter(|&destination| targets[destination as usize]);
            if let Some(destination) = names.next() {
                text.push_str(destination.name());
            }
            for destination in names {
                text.push(',');
                text.push_str(destination.name());
            }
            text.push('\n');
        }
        let _ = writeln!(text, "events = {}", self.kind_names().join(","));
        let _ = writeln!(text, "content = {}", self.content.name());
        if !self.telegram_chat.is_empty() {
            let _ = writeln!(text, "telegram_chat = {}", self.telegram_chat);
        }
        text
    }

    fn targets(&self) -> [bool; 2] {
        self.targets.unwrap_or_else(|| Destination::ALL.map(|destination| self.destination == Some(destination)))
    }

    /// The targets a legacy `milo alerts on` would enable.
    pub(crate) fn enable_targets(&self) -> [bool; 2] {
        let mut targets = self.targets();
        if !targets.contains(&true) && let Some(destination) = self.destination {
            targets[destination as usize] = true;
        }
        targets
    }

    pub(crate) fn active(&self, destination: Destination) -> bool {
        self.enabled && self.targets()[destination as usize]
    }

    fn has_active(&self) -> bool {
        self.enabled && self.targets().contains(&true)
    }

    fn remove_target(&mut self, destination: Destination) {
        if let Some(targets) = &mut self.targets {
            targets[destination as usize] = false;
            self.enabled &= targets.contains(&true);
        } else if self.destination == Some(destination) {
            self.enabled = false;
        }
    }

    pub(crate) fn mirrors(&self, kind: Kind) -> bool {
        self.kinds[kind as usize]
    }

    fn kind_names(&self) -> Vec<&'static str> {
        Kind::ALL.into_iter().filter(|&kind| self.mirrors(kind)).map(Kind::name).collect()
    }
}

/// A comma-separated list of kinds, which may be empty.
pub(crate) fn parse_kinds(list: &str) -> Option<[bool; 5]> {
    let mut kinds = [false; 5];
    for name in list.split(',').map(str::trim).filter(|name| !name.is_empty()) {
        kinds[Kind::parse(name)? as usize] = true;
    }
    Some(kinds)
}

/// A chat's number, negative for groups and channels, or a public `@name`.
fn valid_chat(chat: &str) -> bool {
    let digits = |text: &str| (1..=20).contains(&text.len()) && text.bytes().all(|c| c.is_ascii_digit());
    match chat.strip_prefix('@') {
        Some(name) => {
            (4..=32).contains(&name.len())
                && name.as_bytes()[0].is_ascii_alphabetic()
                && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        }
        None => digits(chat.strip_prefix('-').unwrap_or(chat)),
    }
}

/// A bot token as BotFather gives it: the bot's number, a colon, and its key.
fn valid_token(token: &str) -> bool {
    token.split_once(':').is_some_and(|(bot, key)| {
        (3..=20).contains(&bot.len())
            && bot.bytes().all(|c| c.is_ascii_digit())
            && (20..=80).contains(&key.len())
            && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    })
}

/// The `ID/TOKEN` of a Discord webhook link. Only Discord's own hosts and
/// the plain link are accepted, so a saved secret can never be sent elsewhere.
fn webhook(link: &str) -> Option<&str> {
    let path = ["https://discord.com/api/webhooks/", "https://discordapp.com/api/webhooks/"]
        .into_iter()
        .find_map(|host| link.strip_prefix(host))?;
    valid_webhook_path(path).then_some(path)
}

fn valid_webhook_path(path: &str) -> bool {
    let Some((id, token)) = path.split_once('/') else { return false };
    (5..=25).contains(&id.len())
        && id.bytes().all(|c| c.is_ascii_digit())
        && (20..=100).contains(&token.len())
        && token.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// The address of a stand-in, if `named` is one on this machine's loopback
/// address. Nothing else is accepted, so secrets cannot be sent elsewhere.
fn stand_in(named: &str) -> Option<&str> {
    let port = named.strip_prefix("http://127.0.0.1:")?;
    port.parse::<u16>().is_ok().then_some(named)
}

/// What the settings pages show. It never holds a secret.
pub(crate) struct Summary {
    pub(crate) settings: Settings,
    /// Whether a secret is saved for each destination, in `Destination::ALL` order.
    pub(crate) saved: [bool; 2],
    pub(crate) telegram_needs_local_forget: bool,
    pub(crate) running: bool,
    pub(crate) note: String,
    pub(crate) consent: Consent,
}

impl Summary {
    pub(crate) fn saved(&self, destination: Destination) -> bool {
        self.saved[destination as usize]
    }
}

/// Starts mirroring again if the user left it on and nothing is mirroring.
/// Rex's listener is started each time the server loads its settings, which
/// makes this the way mirroring returns after Rex or the machine restarts.
pub(crate) fn resume() {
    if let Some(alerts) = Alerts::real() && alerts.set_up() {
        let _ = alerts.locked(|edit| {
            if edit.settings().is_ok_and(|settings| settings.has_active())
                && !lock::alerts_running(&alerts.state).unwrap_or(true)
            {
                alerts.start()?;
            }
            Ok(())
        });
    }
}

/// Ends mirroring and removes every saved destination and its secret, as
/// removing Milo's integrations does. The settings are not read, so that a
/// broken file cannot keep anything in place. Nothing is asked of the
/// Keychain for a user who never set mirroring up.
pub(crate) fn remove() -> Result<(), String> {
    let Some(alerts) = Alerts::real() else { return Ok(()) };
    if alerts.set_up() { alerts.locked(|edit| edit.remove()) } else { Ok(()) }
}

/// Milo's alert settings, the secrets beside them, and the mirroring process.
pub(crate) struct Alerts {
    config: PathBuf,
    state: PathBuf,
    secrets: secret::Store,
    /// A test's stand-in for both destinations.
    stand_in: Option<String>,
    relay: Option<relay::Origin>,
}

const TELEGRAM_LOCAL_FORGET: &str =
    "The saved Telegram connection belongs to a different relay environment and cannot be disconnected here. If the \
     original relay is gone, use Forget on this Mac in Settings or milo alerts forget telegram --local-only before \
     reconnecting.";

/// A settings and credential transaction. All edits, including a reviewed
/// change's comparison and commit, use the same configuration directory lock.
pub(crate) struct AlertsEdit<'a> {
    alerts: &'a Alerts,
    dir: &'a Path,
    file: &'a Path,
}

impl Alerts {
    pub(crate) fn real() -> Option<Self> {
        let (config, state) = (paths::config_dir()?, paths::state_dir()?);
        let secrets = secret::Store::real(&state);
        let named = cfg!(debug_assertions).then(|| std::env::var(TEST_ENDPOINT).ok()).flatten();
        let stand_in = named.and_then(|named| stand_in(&named).map(str::to_owned));
        Some(Self { config, state, secrets, stand_in, relay: relay::Origin::configured() })
    }

    /// Settings, state and a secrets file in folders of a test's own.
    #[cfg(test)]
    pub(crate) fn at(config: &Path, state: &Path) -> Self {
        let secrets = secret::Store::File(state.join("alerts-secrets"));
        Self {
            config: config.to_owned(),
            state: state.to_owned(),
            secrets,
            stand_in: None,
            relay: Some(relay::Origin::Local),
        }
    }

    /// Build the whole request once, including the destination's wire format,
    /// after checking which service is allowed to receive the saved secret.
    fn message_request(
        &self,
        destination: Destination,
        secret: &str,
        chat: &str,
        text: &str,
        kind: Option<Kind>,
    ) -> Option<send::Request> {
        let base = self.stand_in.as_deref();
        let address = match destination {
            Destination::Telegram => {
                if let Some(capability) = self.telegram_capability(secret, chat).ok()? {
                    return self.relay.as_ref()?.send_request(capability, text);
                }
                format!("{}/bot{secret}/sendMessage", base.unwrap_or(telegram::API))
            }
            Destination::Discord if valid_webhook_path(secret) => {
                format!("{}/api/v10/webhooks/{secret}", base.unwrap_or("https://discord.com"))
            }
            Destination::Discord => return None,
        };
        Some(send::request(&address, &message::body(destination, chat, text, kind)))
    }

    fn file(&self) -> PathBuf {
        self.config.join("alerts.conf")
    }

    /// Whether the user ever saved an alert choice. Until then nothing is
    /// asked of the Keychain.
    fn set_up(&self) -> bool {
        self.file().try_exists().unwrap_or(true)
    }

    /// The saved settings, or the defaults, which mirror nothing.
    pub(crate) fn settings(&self) -> io::Result<Settings> {
        match preferences::read(&self.file(), SETTINGS_LIMIT)? {
            Some(text) => Settings::parse(&text),
            None => Ok(Settings::default()),
        }
    }

    pub(crate) fn locked<T>(&self, action: impl FnOnce(&AlertsEdit<'_>) -> Result<T, String>) -> Result<T, String> {
        let file = self.file();
        preferences::locked(&file, |dir| Ok(action(&AlertsEdit { alerts: self, dir, file: &file })))
            .map_err(|_| "Could not lock Milo's alert settings.".to_owned())?
    }

    /// Ends the mirroring process, if one is running.
    fn stop(&self) -> Result<(), String> {
        lock::stop_alerts(&self.state).map_err(|_| "Could not stop the process mirroring alerts.".to_owned())?;
        // What the ended process last noted says nothing about the next one.
        forward::forget_note(&self.state);
        Ok(())
    }

    fn start_if_active(&self, settings: &Settings) -> Result<(), String> {
        if settings.has_active() { self.start() } else { Ok(()) }
    }

    fn telegram_capability<'a>(&self, secret: &'a str, chat: &str) -> Result<Option<&'a str>, &'static str> {
        let Some((capability, verified_chat)) = secret::telegram_binding(secret) else {
            return if valid_token(secret) { Ok(None) } else { Err(REPAIR_TELEGRAM) };
        };
        if !self.relay.as_ref().is_some_and(|relay| relay.accepts(capability)) {
            return Err(TELEGRAM_LOCAL_FORGET);
        }
        if verified_chat != Some(chat) || !relay::valid_chat(chat) {
            return Err(REPAIR_TELEGRAM);
        }
        Ok(Some(capability))
    }

    fn secret(&self, destination: Destination) -> Result<String, String> {
        match self.secrets.get(destination) {
            Ok(Some(secret)) => Ok(secret),
            Ok(None) => Err(Self::no_saved_secret(destination)),
            Err(_) => Err(self.secret_read_error()),
        }
    }

    fn secret_read_error(&self) -> String {
        format!("Could not read the saved secret from {}.", self.secrets.place())
    }

    fn target_secret(&self, settings: &Settings, destination: Destination) -> Result<String, String> {
        let secret = self.secret(destination)?;
        self.validate_target_secret(settings, destination, &secret)?;
        Ok(secret)
    }

    fn validate_target_secret(&self, settings: &Settings, destination: Destination, secret: &str) -> Result<(), String> {
        if destination == Destination::Telegram {
            self.telegram_capability(secret, &settings.telegram_chat)?;
            if settings.telegram_chat.is_empty() {
                return Err("No Telegram chat is saved. Save it again with milo alerts telegram CHAT.".into());
            }
        } else if !valid_webhook_path(secret) {
            return Err("The saved Discord secret is invalid. Save it again.".into());
        }
        Ok(())
    }

    fn no_saved_secret(destination: Destination) -> String {
        format!("No {} destination is saved. {NO_DESTINATION}", destination.title())
    }

    /// Starts the mirroring process in the background, apart from this
    /// terminal and folder, so that closing either does not end or hold it.
    pub(crate) fn start(&self) -> Result<(), String> {
        const FAILED: &str = "Settings saved, but mirroring did not start. Run milo alerts to see why.";
        if cfg!(test) {
            return Ok(());
        }
        let milo = std::env::current_exe().map_err(|_| FAILED.to_owned())?;
        let mut command = Command::new(milo);
        command.args(["alerts", "run"]).current_dir("/");
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(path) = crate::rex::path_with_rex() {
            command.env("PATH", path);
        }
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut process = command.spawn().map_err(|_| FAILED.to_owned())?;
        // Whenever it ends, it is waited for, so that it does not linger in a
        // settings page that stays open.
        std::thread::spawn(move || process.wait());
        match lock::wait_alerts_running(&self.state) {
            Ok(true) => Ok(()),
            _ => Err(FAILED.into()),
        }
    }

    pub(crate) fn summary(&self) -> Result<Summary, String> {
        self.locked(|edit| edit.summary())
    }

    /// Saves a manual Telegram bot token and chat.
    pub(crate) fn set_telegram(&self, token: &str, chat: &str) -> Result<String, String> {
        if !valid_chat(chat) {
            return Err("CHAT must be a chat's number, such as -1001234567890, or a public channel's @name.".into());
        }
        if !valid_token(token) {
            if relay::valid_capability(token) {
                return Err(REPAIR_TELEGRAM.into());
            }
            return Err("That is not a bot token. BotFather gives one that looks like 123456:ABC-DEF…".into());
        }
        self.locked(|edit| edit.keep(Destination::Telegram, token, Some(chat), false))
    }

    #[cfg(test)]
    fn set_hosted_telegram(&self, token: &str, chat: &str) -> Result<String, String> {
        self.locked(|edit| edit.set_hosted_telegram(token, chat))
    }

    /// Finds recent Telegram chats without saving a token or changing subscriptions.
    pub(crate) fn telegram_chats(&self, token: &str) -> Result<Vec<(i64, String)>, TelegramChatsError> {
        if !valid_token(token) {
            return Err(TelegramChatsError::InvalidToken);
        }
        telegram::discover(self.stand_in.as_deref(), token).ok_or(TelegramChatsError::Unavailable)
    }

    /// Saves a Discord webhook link.
    pub(crate) fn set_discord(&self, link: &str) -> Result<String, String> {
        self.locked(|edit| edit.save_discord(link, false))
    }

    #[cfg(test)]
    fn set_hosted_discord(&self, link: &str) -> Result<String, String> {
        self.locked(|edit| edit.set_hosted_discord(link))
    }

    /// Mirrors one kind of alert, or stops mirroring it.
    pub(crate) fn set_kind(&self, kind: Kind, on: bool) -> Result<String, String> {
        self.locked(|edit| {
            let mut settings = edit.editable()?;
            settings.kinds[kind as usize] = on;
            edit.save(&settings)?;
            Ok(message::disclosure(&settings))
        })
    }

    /// Where secrets are kept, to tell the user.
    pub(crate) fn place(&self) -> &'static str {
        self.secrets.place()
    }

    pub(crate) fn choose(&self, destination: Destination) -> Result<String, String> {
        self.locked(|edit| edit.choose(destination))
    }

    pub(crate) fn set_kinds(&self, kinds: [bool; 5]) -> Result<String, String> {
        self.locked(|edit| edit.set_kinds(kinds))
    }

    pub(crate) fn set_content(&self, content: Content) -> Result<String, String> {
        self.locked(|edit| edit.set_content(content))
    }

    pub(crate) fn enable(&self) -> Result<String, String> {
        self.locked(|edit| edit.enable())
    }

    pub(crate) fn set_forwarding(&self, destination: Destination, enabled: bool) -> Result<String, String> {
        self.locked(|edit| edit.set_forwarding(destination, enabled))
    }

    /// Removes a saved Telegram connection from this machine without asking a relay to revoke it.
    fn forget_telegram_locally(&self) -> Result<String, String> {
        self.locked(|edit| edit.forget_telegram_locally())
    }

    /// A browser confirmation for an incompatible binding must not discard a newer, compatible one.
    pub(crate) fn forget_incompatible_telegram(&self) -> Result<String, String> {
        self.locked(|edit| {
            if !edit.telegram_needs_local_forget()? {
                return Err(
                    "There is no incompatible Telegram connection to forget. Review the saved destination again.".into()
                );
            }
            edit.forget_telegram_locally()
        })
    }

    pub(crate) fn check_telegram_reconnect(&self) -> Result<(), String> {
        if self.set_up() { self.locked(|edit| edit.check_telegram_reconnect()) } else { Ok(()) }
    }

    pub(crate) fn disable(&self) -> Result<String, String> {
        self.locked(|edit| edit.disable())
    }

    pub(crate) fn forget(&self, destination: Destination) -> Result<String, String> {
        self.locked(|edit| edit.forget(destination))
    }

    /// Sends one message Milo wrote itself to the selected destination.
    pub(crate) fn test(&self) -> Result<String, String> {
        self.locked(|edit| {
            let settings = edit.editable()?;
            self.send_test(&settings, settings.destination.ok_or(NO_DESTINATION)?)
        })
    }

    /// Tests a saved provider whether or not it is selected or active.
    pub(crate) fn test_destination(&self, destination: Destination) -> Result<String, String> {
        self.locked(|edit| self.send_test(&edit.editable()?, destination))
    }

    fn send_test(&self, settings: &Settings, destination: Destination) -> Result<String, String> {
        let secret = self.target_secret(settings, destination)?;
        let request = self
            .message_request(destination, &secret, &settings.telegram_chat, message::TEST, None)
            .ok_or_else(|| format!("The saved {} secret is invalid. Save it again.", destination.title()))?;
        let hosted = destination == Destination::Telegram && secret::telegram_binding(&secret).is_some();
        if hosted && self.settings().map_err(|_| UNREADABLE.to_owned())?.telegram_chat != settings.telegram_chat {
            return Err(REPAIR_TELEGRAM.into());
        }
        match send::post(&request, None) {
            send::Outcome::Sent => {
                Ok(format!("{} accepted the test message.", describe_destination(settings, destination)))
            }
            outcome @ send::Outcome::Uncertain(_) => Err(format!("{}.", outcome.reason(destination, hosted))),
            outcome => {
                Err(format!("The test message was not delivered: {}.", outcome.reason(destination, hosted)))
            }
        }
    }

    fn state_dir(&self) -> &Path {
        &self.state
    }
}

impl AlertsEdit<'_> {
    pub(crate) fn settings(&self) -> io::Result<Settings> {
        self.alerts.settings()
    }

    fn editable(&self) -> Result<Settings, String> {
        self.settings().map_err(|_| UNREADABLE.to_owned())
    }

    pub(crate) fn saved_secret(&self, destination: Destination) -> io::Result<Option<String>> {
        self.alerts.secrets.get(destination)
    }

    fn write_settings(&self, settings: &Settings) -> Result<(), String> {
        preferences::replace(self.dir, self.file, &settings.render()).map_err(|_| UNSAVED.to_owned())
    }

    fn save(&self, settings: &Settings) -> Result<(), String> {
        self.alerts.stop()?;
        self.write_settings(settings)?;
        self.alerts.start_if_active(settings)
    }

    pub(crate) fn summary(&self) -> Result<Summary, String> {
        let settings = self.editable()?;
        let set_up = self.alerts.set_up();
        let credentials =
            Destination::ALL.map(|destination| if set_up { self.saved_secret(destination).ok().flatten() } else { None });
        let saved = credentials.each_ref().map(Option::is_some);
        let telegram_needs_local_forget =
            self.incompatible_telegram_binding(credentials[Destination::Telegram as usize].as_deref());
        let consent = Consent::at(&settings, credentials.each_ref().map(|secret| secret.as_deref()));
        let running = lock::alerts_running(&self.alerts.state).unwrap_or(false);
        let mut note = forward::note(&self.alerts.state);
        if let Some(secret) = credentials[Destination::Telegram as usize].as_deref()
            && let Err(error) = self.alerts.telegram_capability(secret, &settings.telegram_chat)
            && !note.contains(error)
        {
            if !note.is_empty() {
                note.push('\n');
            }
            note.push_str(error);
        }
        if self.alerts.stand_in.is_some() {
            note.insert_str(0, "Messages go to a test stand-in on this machine, not to Telegram or Discord.\n");
        }
        Ok(Summary { settings, saved, telegram_needs_local_forget, running, note, consent })
    }

    fn incompatible_telegram_binding(&self, saved: Option<&str>) -> bool {
        // Older production connections used the same prefix as loopback. They must never
        // be sent to another relay (or passed off as revoked).
        saved.and_then(secret::telegram_binding).is_some_and(|(capability, _)| {
            !self.alerts.relay.as_ref().is_some_and(|relay| relay.accepts(capability))
        })
    }

    fn telegram_needs_local_forget(&self) -> Result<bool, String> {
        self.saved_secret(Destination::Telegram)
            .map(|saved| self.incompatible_telegram_binding(saved.as_deref()))
            .map_err(|_| self.alerts.secret_read_error())
    }

    pub(crate) fn check_telegram_reconnect(&self) -> Result<(), String> {
        if self.telegram_needs_local_forget()? { Err(TELEGRAM_LOCAL_FORGET.into()) } else { Ok(()) }
    }

    /// Only called with a trusted relay result, never with a browser-supplied chat.
    pub(crate) fn set_hosted_telegram(&self, token: &str, chat: &str) -> Result<String, String> {
        if !relay::valid_capability(token) {
            return Err("Invalid hosted Telegram connection. Connect Telegram again.".into());
        }
        if !self.alerts.relay.as_ref().is_some_and(|relay| relay.accepts(token)) {
            return Err(
                "This Telegram connection belongs to a different relay environment. Check how Milo was started.".into(),
            );
        }
        if !relay::valid_chat(chat) {
            return Err("The connected Telegram destination must be a private chat with Milo's bot.".into());
        }
        // Keep the verified recipient in the same secret entry as the capability.
        self.keep(Destination::Telegram, &format!("{token}:{chat}"), Some(chat), true)
    }

    pub(crate) fn set_hosted_discord(&self, link: &str) -> Result<String, String> {
        self.save_discord(link, true)
    }

    /// Whether it is safe to revoke a newly claimed connection after a failed save.
    pub(crate) fn hosted_save_absent(&self, destination: Destination, credential: &str) -> bool {
        match self.saved_secret(destination) {
            Ok(None) => true,
            Ok(Some(saved)) => match destination {
                Destination::Telegram => {
                    secret::telegram_binding(&saved).is_some_and(|(capability, _)| capability != credential)
                        || valid_token(&saved)
                }
                Destination::Discord => {
                    relay::hosted_webhook(credential).is_some_and(|path| valid_webhook_path(&saved) && saved != path)
                }
            },
            Err(_) => false,
        }
    }

    fn save_discord(&self, link: &str, hosted: bool) -> Result<String, String> {
        let Some(secret) = (if hosted { relay::hosted_webhook(link) } else { webhook(link) }) else {
            return Err("That is not a Discord webhook link. Copy it from the channel's Integrations settings: \
                        https://discord.com/api/webhooks/…"
                .into());
        };
        self.keep(Destination::Discord, secret, None, hosted)
    }

    /// Only an identical manual re-save retains the destination's forwarding choice.
    fn keep(&self, destination: Destination, secret: &str, chat: Option<&str>, hosted: bool) -> Result<String, String> {
        let mut settings = self.editable()?;
        let previous = self.saved_secret(destination).map_err(|_| self.alerts.secret_read_error())?;
        if destination == Destination::Telegram && self.incompatible_telegram_binding(previous.as_deref()) {
            return Err(TELEGRAM_LOCAL_FORGET.into());
        }
        let was_active = settings.active(destination);
        let changed = previous.as_deref() != Some(secret);
        let new_chat = chat.is_some_and(|chat| settings.telegram_chat != chat);
        let select = !settings.has_active() || settings.destination.is_none();
        if (hosted || changed || new_chat)
            && (was_active || settings.targets.is_some_and(|targets| targets[destination as usize]))
        {
            settings.remove_target(destination);
            self.write_settings(&settings)?;
        }
        if was_active {
            self.alerts.stop()?;
        }
        let disarmed = !settings.active(destination);
        let restarted = if was_active && disarmed { self.alerts.start_if_active(&settings) } else { Ok(()) };
        let kept = (|| {
            if destination == Destination::Telegram {
                self.revoke_telegram(previous.as_deref(), Some(secret))?;
            }
            if select {
                settings.destination = Some(destination);
            }
            if let Some(chat) = chat {
                chat.clone_into(&mut settings.telegram_chat);
            }
            self.write_settings(&settings)?;
            if changed {
                self.alerts
                    .secrets
                    .set(destination, secret)
                    .map_err(|_| format!("Could not save the secret in {}.", self.alerts.place()))?;
            }
            Ok(())
        })();
        if was_active && !disarmed {
            kept.and(self.alerts.start_if_active(&settings))?;
        } else {
            kept.and(restarted)?;
        }
        let saved =
            format!("Saved the {} destination; its secret is in {}.", destination.title(), self.alerts.place());
        let name = destination.name();
        Ok(if settings.active(destination) {
            format!("{saved} Alerts go to {}.", describe_active(&settings))
        } else if settings.has_active() {
            format!(
                "{saved} Alerts still go to {}. Turn this destination on with milo alerts on {name}.",
                describe_active(&settings)
            )
        } else {
            format!(
                "{saved} Mirroring is still off. Check it with milo alerts test {name}, then run milo alerts on {name}."
            )
        })
    }

    fn revoke_telegram(&self, saved: Option<&str>, replacement: Option<&str>) -> Result<(), String> {
        if let Some((capability, _)) = saved.and_then(secret::telegram_binding)
            && replacement.and_then(secret::telegram_binding).map(|(capability, _)| capability) != Some(capability)
            && !self.alerts.relay.as_ref().is_some_and(|relay| relay.revoke(capability))
        {
            return Err(
                "Could not disconnect the saved Telegram connection. Retry in its original relay environment before \
                 connecting Telegram again. If that relay is gone for good, milo alerts forget telegram --local-only \
                 removes the connection from this machine without revoking it."
                    .into(),
            );
        }
        Ok(())
    }

    fn revoke_and_discard(&self, destination: Destination) -> Result<(), String> {
        if destination == Destination::Telegram {
            let saved = self.saved_secret(destination).map_err(|_| self.alerts.secret_read_error())?;
            self.revoke_telegram(saved.as_deref(), None)?;
        }
        self.discard(destination)
    }

    fn discard(&self, destination: Destination) -> Result<(), String> {
        self.alerts
            .secrets
            .remove(destination)
            .map_err(|_| format!("Could not remove the secret from {}.", self.alerts.place()))
    }

    fn remove(&self) -> Result<(), String> {
        // A failed revoke or deletion must not leave a configuration that can resume.
        self.write_settings(&Settings::default())?;
        self.alerts.stop()?;
        for destination in Destination::ALL {
            self.revoke_and_discard(destination)?;
        }
        std::fs::remove_file(self.file).map_err(|_| "Could not remove Milo's alert settings (alerts.conf).".to_owned())
    }

    /// Selects a saved destination for legacy commands. When no independent
    /// choices were made, this also changes where active mirroring goes.
    fn choose(&self, destination: Destination) -> Result<String, String> {
        let mut settings = self.editable()?;
        self.alerts.target_secret(&settings, destination)?;
        settings.destination = Some(destination);
        if settings.targets.is_some() {
            self.write_settings(&settings)?;
        } else {
            self.save(&settings)?;
        }
        let mut report = format!("Selected {}.", describe(&settings));
        if settings.has_active() {
            let _ = write!(report, " Alerts go to {}.", describe_active(&settings));
        } else {
            report.push_str(" Mirroring is still off. Check the destination with milo alerts test.");
        }
        Ok(report)
    }

    fn set_kinds(&self, kinds: [bool; 5]) -> Result<String, String> {
        let mut settings = self.editable()?;
        settings.kinds = kinds;
        self.save(&settings)?;
        Ok(message::disclosure(&settings))
    }

    /// Chooses what is sent, and says it first of all in the answer.
    fn set_content(&self, content: Content) -> Result<String, String> {
        let mut settings = self.editable()?;
        settings.content = content;
        self.save(&settings)?;
        Ok(message::sends(content).to_owned())
    }

    /// Resumes the chosen targets, or starts with the legacy selection.
    pub(crate) fn enable(&self) -> Result<String, String> {
        self.enable_with(self.editable()?, None)
    }

    pub(super) fn enable_with(&self, mut settings: Settings, reviewed: Option<[Option<&str>; 2]>) -> Result<String, String> {
        let targets = settings.enable_targets();
        if !targets.contains(&true) {
            return Err(NO_DESTINATION.into());
        }
        if targets != settings.targets() {
            settings.targets = Some(targets);
        }
        for destination in Destination::ALL.into_iter().filter(|&destination| targets[destination as usize]) {
            self.check_target_secret(&settings, destination, reviewed)?;
        }
        settings.enabled = true;
        self.save(&settings)?;
        Ok(format!(
            "Rex alert mirroring is on. Alerts go to {}.\n{}\nRex's own notifications are unchanged. Turn this off \
             with milo alerts off.",
            describe_active(&settings),
            message::disclosure(&settings)
        ))
    }

    /// Turns one provider on or off without changing the legacy selection.
    pub(crate) fn set_forwarding(&self, destination: Destination, enabled: bool) -> Result<String, String> {
        self.set_forwarding_with(self.editable()?, destination, enabled, None)
    }

    pub(super) fn set_forwarding_with(
        &self,
        mut settings: Settings,
        destination: Destination,
        enabled: bool,
        reviewed: Option<[Option<&str>; 2]>,
    ) -> Result<String, String> {
        let index = destination as usize;
        if enabled {
            self.check_target_secret(&settings, destination, reviewed)?;
            let mut targets = if settings.enabled { settings.targets() } else { [false; 2] };
            targets[index] = true;
            settings.targets = Some(targets);
            settings.enabled = true;
            self.save(&settings)?;
        } else if settings.active(destination) || settings.targets.is_some_and(|targets| targets[index]) {
            settings.targets = Some(settings.targets());
            settings.remove_target(destination);
            self.save(&settings)?;
        }
        let state = if enabled { "on" } else { "off" };
        let mut report = format!("{} alert mirroring is {state}.", destination.title());
        if enabled {
            let _ = write!(report, "\n{}", message::disclosure(&settings));
        }
        Ok(report)
    }

    fn check_target_secret(
        &self,
        settings: &Settings,
        destination: Destination,
        reviewed: Option<[Option<&str>; 2]>,
    ) -> Result<(), String> {
        if let Some(reviewed) = reviewed {
            let secret = reviewed[destination as usize].ok_or_else(|| Alerts::no_saved_secret(destination))?;
            self.alerts.validate_target_secret(settings, destination, secret)
        } else {
            self.alerts.target_secret(settings, destination).map(drop)
        }
    }

    /// Turns mirroring off and ends the mirroring process before returning.
    /// Settings that cannot be read are replaced: turning off always works.
    fn disable(&self) -> Result<String, String> {
        let mut settings = self.settings().unwrap_or_default();
        settings.enabled = false;
        self.write_settings(&settings)?;
        self.alerts.stop()?;
        Ok("Rex alert mirroring is off. Nothing more is sent. Saved destinations were kept; remove one with milo \
            alerts forget telegram or milo alerts forget discord."
            .into())
    }

    /// Removes a saved destination and its secret.
    fn forget(&self, destination: Destination) -> Result<String, String> {
        self.disarm_then(destination, || self.revoke_and_discard(destination))?;
        Ok(format!("Removed the saved {} destination and its secret.", destination.title()))
    }

    /// The way out when the relay that issued a hosted Telegram connection is
    /// gone for good, so that no revocation can ever be confirmed. No relay is
    /// asked anything and the connection is not revoked: the answer says so.
    /// `forget` and `remove` never fall back to this; only the user chooses it.
    fn forget_telegram_locally(&self) -> Result<String, String> {
        const UNREVOKED: &str = "Removed the saved Telegram connection from this machine only. No relay was asked to \
                                 revoke it, and nothing proves that it is revoked. To revoke every Milo connection to \
                                 that chat, send /stop to Milo's bot in Telegram.";
        let telegram = Destination::Telegram;
        let mut unrevoked = false;
        let ended = self.disarm_then(telegram, || {
            let saved = self.saved_secret(telegram).map_err(|_| self.alerts.secret_read_error())?;
            self.discard(telegram)?;
            unrevoked = saved.as_deref().and_then(secret::telegram_binding).is_some();
            Ok(())
        });
        // The warning is owed once the connection is gone, even if mirroring elsewhere did not start again.
        match (ended, unrevoked) {
            (Ok(()), true) => Ok(UNREVOKED.into()),
            (Ok(()), false) => Ok(format!("Removed the saved {} destination and its secret.", telegram.title())),
            (Err(error), true) => Err(format!("{UNREVOKED}\n{error}")),
            (Err(error), false) => Err(error),
        }
    }

    /// Ends mirroring to a destination and saves that before `discard` takes
    /// its secret away, so that a removal that fails leaves it off. Settings
    /// that cannot be read do not stand in the way.
    fn disarm_then(
        &self,
        destination: Destination,
        discard: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let read = self.settings();
        let unreadable = read.is_err();
        let mut settings = read.unwrap_or_default();
        let was_active = settings.active(destination);
        let selected = settings.destination == Some(destination);
        settings.remove_target(destination);
        if selected {
            settings.destination = None;
        }
        if destination == Destination::Telegram {
            settings.telegram_chat.clear();
        }
        let restart = was_active || unreadable;
        self.write_settings(&settings)?;
        if restart {
            self.alerts.stop()?;
        }
        // Restart unrelated targets even when this one's secret cannot be revoked or removed.
        let restarted = if restart { self.alerts.start_if_active(&settings) } else { Ok(()) };
        discard().and(restarted)
    }
}

/// The selected destination in words, without its secret.
pub(crate) fn describe(settings: &Settings) -> String {
    settings.destination.map_or_else(|| "no destination".into(), |destination| {
        describe_destination(settings, destination).into_owned()
    })
}

fn describe_destination(settings: &Settings, destination: Destination) -> std::borrow::Cow<'static, str> {
    match destination {
        Destination::Telegram => format!("Telegram chat {}", settings.telegram_chat).into(),
        Destination::Discord => "the Discord channel of the saved webhook".into(),
    }
}

fn describe_active(settings: &Settings) -> String {
    let mut destinations = Destination::ALL.into_iter().filter(|&destination| settings.active(destination));
    let Some(first) = destinations.next() else { return "no destination".into() };
    let mut description = describe_destination(settings, first).into_owned();
    for destination in destinations {
        description.push_str(" and ");
        description.push_str(&describe_destination(settings, destination));
    }
    description
}
