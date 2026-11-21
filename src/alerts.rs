//! Mirrors Rex alerts to the Telegram and Discord destinations the user chose.
//!
//! An *alert* is something Rex itself draws attention to: a program that says
//! through its terminal that it is blocked, failed or finished, one that asks
//! for a desktop notification, or one that rings the bell. Milo hears these on
//! Rex's event stream, beside Rex, so Rex's own notifications are untouched.
//! Mirroring is off until the user turns it on, and sends the kind of alert
//! and the program's short name only, unless the user asks for its text too.

mod cli;
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
    pub(crate) running: bool,
    pub(crate) note: String,
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
    if let Some(alerts) = Alerts::real()
        && alerts.settings().is_ok_and(|settings| settings.has_active())
        && !lock::alerts_running(&alerts.state).unwrap_or(true)
    {
        let _ = alerts.start();
    }
}

/// Ends mirroring and removes every saved destination and its secret, as
/// removing Milo's integrations does. The settings are not read, so that a
/// broken file cannot keep anything in place. Nothing is asked of the
/// Keychain for a user who never set mirroring up.
pub(crate) fn remove() -> Result<(), String> {
    let Some(alerts) = Alerts::real() else { return Ok(()) };
    if !alerts.set_up() {
        return Ok(());
    }
    alerts.stop()?;
    for destination in Destination::ALL {
        alerts.revoke_saved(destination, None)?;
        alerts.discard(destination)?;
    }
    std::fs::remove_file(alerts.file()).map_err(|_| "Could not remove Milo's alert settings (alerts.conf).".to_owned())
}

/// Milo's alert settings, the secrets beside them, and the mirroring process.
pub(crate) struct Alerts {
    config: PathBuf,
    state: PathBuf,
    secrets: secret::Store,
    /// A test's stand-in for both destinations.
    stand_in: Option<String>,
}

impl Alerts {
    pub(crate) fn real() -> Option<Self> {
        let (config, state) = (paths::config_dir()?, paths::state_dir()?);
        let secrets = secret::Store::real(&state);
        let named = cfg!(debug_assertions).then(|| std::env::var(TEST_ENDPOINT).ok()).flatten();
        let stand_in = named.and_then(|named| stand_in(&named).map(str::to_owned));
        Some(Self { config, state, secrets, stand_in })
    }

    /// Settings, state and a secrets file in folders of a test's own.
    #[cfg(test)]
    pub(crate) fn at(config: &Path, state: &Path) -> Self {
        let secrets = secret::Store::File(state.join("alerts-secrets"));
        Self { config: config.to_owned(), state: state.to_owned(), secrets, stand_in: None }
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
    ) -> Option<String> {
        let base = self.stand_in.as_deref();
        let address = match destination {
            Destination::Telegram if relay::valid_capability(secret) => {
                return Some(relay::send_request(base, secret, text));
            }
            Destination::Telegram if valid_token(secret) => {
                format!("{}/bot{secret}/sendMessage", base.unwrap_or(telegram::API))
            }
            Destination::Discord if valid_webhook_path(secret) => {
                format!("{}/api/v10/webhooks/{secret}", base.unwrap_or("https://discord.com"))
            }
            _ => return None,
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

    fn editable(&self) -> Result<Settings, String> {
        self.settings().map_err(|_| UNREADABLE.to_owned())
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

    fn write_settings(&self, settings: &Settings) -> Result<(), String> {
        let file = self.file();
        preferences::locked(&file, |dir| preferences::replace(dir, &file, &settings.render()))
            .map_err(|_| UNSAVED.to_owned())
    }

    fn save(&self, settings: &Settings) -> Result<(), String> {
        self.stop()?;
        self.write_settings(settings)?;
        self.start_if_active(settings)
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
        let settings = self.editable()?;
        let saved =
            Destination::ALL.map(|destination| self.set_up() && matches!(self.secrets.get(destination), Ok(Some(_))));
        let running = lock::alerts_running(&self.state).unwrap_or(false);
        let mut note = forward::note(&self.state);
        if self.stand_in.is_some() {
            note.insert_str(0, "Messages go to a test stand-in on this machine, not to Telegram or Discord.\n");
        }
        Ok(Summary { settings, saved, running, note })
    }

    /// Saves a manual bot token or the capability for a verified Telegram DM.
    pub(crate) fn set_telegram(&self, token: &str, chat: &str) -> Result<String, String> {
        self.save_telegram(token, chat, false)
    }

    pub(crate) fn set_hosted_telegram(&self, token: &str, chat: &str) -> Result<String, String> {
        self.save_telegram(token, chat, true)
    }

    fn save_telegram(&self, token: &str, chat: &str, require_hosted: bool) -> Result<String, String> {
        let hosted = relay::valid_capability(token);
        if hosted {
            if !relay::valid_chat(chat) {
                return Err("The connected Telegram destination must be a private chat with Milo's bot.".into());
            }
        } else {
            if require_hosted {
                return Err("Invalid hosted Telegram connection. Connect Telegram again.".into());
            }
            if !valid_chat(chat) {
                return Err("CHAT must be a chat's number, such as -1001234567890, or a public channel's @name.".into());
            }
            if !valid_token(token) {
                return Err("That is not a bot token. BotFather gives one that looks like 123456:ABC-DEF…".into());
            }
        }
        let mut settings = self.editable()?;
        chat.clone_into(&mut settings.telegram_chat);
        self.keep(settings, Destination::Telegram, token, hosted)
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
        self.save_discord(link, false)
    }

    pub(crate) fn set_hosted_discord(&self, link: &str) -> Result<String, String> {
        self.save_discord(link, true)
    }

    fn save_discord(&self, link: &str, hosted: bool) -> Result<String, String> {
        let Some(secret) = webhook(link) else {
            return Err("That is not a Discord webhook link. Copy it from the channel's Integrations settings: \
                        https://discord.com/api/webhooks/…"
                .into());
        };
        self.keep(self.editable()?, Destination::Discord, secret, hosted)
    }

    /// Keeps a credential without opting in. A hosted connection must be
    /// turned on explicitly, even when it replaces an active credential.
    fn keep(&self, mut settings: Settings, destination: Destination, secret: &str, hosted: bool) -> Result<String, String> {
        let was_active = settings.active(destination);
        if !settings.has_active() || settings.destination.is_none() {
            settings.destination = Some(destination);
        }
        if hosted {
            settings.remove_target(destination);
        }
        self.revoke_saved(destination, Some(secret))?;
        if was_active {
            self.stop()?;
        }
        self.write_settings(&settings)?;
        let kept = self
            .secrets
            .set(destination, secret)
            .map_err(|_| format!("Could not save the secret in {}.", self.secrets.place()));
        if was_active && (kept.is_ok() || !settings.active(destination)) {
            let restarted = self.start_if_active(&settings);
            kept.and(restarted)?;
        } else {
            kept?;
        }
        let saved =
            format!("Saved the {} destination; its secret is in {}.", destination.title(), self.secrets.place());
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

    fn secret(&self, destination: Destination) -> Result<String, String> {
        match self.secrets.get(destination) {
            Ok(Some(secret)) => Ok(secret),
            Ok(None) => Err(format!("No {} destination is saved. {NO_DESTINATION}", destination.title())),
            Err(_) => Err(format!("Could not read the saved secret from {}.", self.secrets.place())),
        }
    }

    fn target_secret(&self, settings: &Settings, destination: Destination) -> Result<String, String> {
        let secret = self.secret(destination)?;
        if destination == Destination::Telegram && settings.telegram_chat.is_empty() {
            return Err("No Telegram chat is saved. Save it again with milo alerts telegram CHAT.".into());
        }
        Ok(secret)
    }

    fn revoke_saved(&self, destination: Destination, replacement: Option<&str>) -> Result<(), String> {
        if destination != Destination::Telegram {
            return Ok(());
        }
        let secret = self
            .secrets
            .get(destination)
            .map_err(|_| format!("Could not read the saved secret from {}.", self.secrets.place()))?;
        if let Some(secret) = secret.as_deref()
            && Some(secret) != replacement
            && relay::valid_capability(secret)
            && !relay::revoke(self.stand_in.as_deref(), secret)
        {
            return Err(
                "Could not disconnect the saved Telegram connection. Start Milo's connection service and try again."
                    .into(),
            );
        }
        Ok(())
    }

    fn discard(&self, destination: Destination) -> Result<(), String> {
        let removed = self.secrets.remove(destination);
        removed.map_err(|_| format!("Could not remove the secret from {}.", self.secrets.place()))
    }

    /// Mirrors one kind of alert, or stops mirroring it.
    pub(crate) fn set_kind(&self, kind: Kind, on: bool) -> Result<String, String> {
        let mut kinds = self.editable()?.kinds;
        kinds[kind as usize] = on;
        self.set_kinds(kinds)
    }

    /// Where secrets are kept, to tell the user.
    pub(crate) fn place(&self) -> &'static str {
        self.secrets.place()
    }

    /// Selects a saved destination for legacy commands. When no independent
    /// choices were made, this also changes where active mirroring goes.
    pub(crate) fn choose(&self, destination: Destination) -> Result<String, String> {
        let mut settings = self.editable()?;
        self.target_secret(&settings, destination)?;
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

    pub(crate) fn set_kinds(&self, kinds: [bool; 5]) -> Result<String, String> {
        let mut settings = self.editable()?;
        settings.kinds = kinds;
        self.save(&settings)?;
        Ok(message::disclosure(&settings))
    }

    /// Chooses what is sent, and says it first of all in the answer.
    pub(crate) fn set_content(&self, content: Content) -> Result<String, String> {
        let mut settings = self.editable()?;
        settings.content = content;
        self.save(&settings)?;
        Ok(message::sends(content).to_owned())
    }

    /// Resumes the chosen targets, or starts with the legacy selection.
    pub(crate) fn enable(&self) -> Result<String, String> {
        let mut settings = self.editable()?;
        let mut targets = settings.targets();
        if !targets.contains(&true) {
            let destination = settings.destination.ok_or(NO_DESTINATION)?;
            targets[destination as usize] = true;
            settings.targets = Some(targets);
        }
        for destination in Destination::ALL.into_iter().filter(|&destination| targets[destination as usize]) {
            self.target_secret(&settings, destination)?;
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
        let mut settings = self.editable()?;
        let index = destination as usize;
        if enabled {
            self.target_secret(&settings, destination)?;
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

    /// Turns mirroring off and ends the mirroring process before returning.
    /// Settings that cannot be read are replaced: turning off always works.
    pub(crate) fn disable(&self) -> Result<String, String> {
        let mut settings = self.settings().unwrap_or_default();
        settings.enabled = false;
        self.save(&settings)?;
        Ok("Rex alert mirroring is off. Nothing more is sent. Saved destinations were kept; remove one with milo \
            alerts forget telegram or milo alerts forget discord."
            .into())
    }

    /// Removes a saved destination and its secret. Mirroring to it ends
    /// first, and settings that cannot be read do not stand in the way.
    pub(crate) fn forget(&self, destination: Destination) -> Result<String, String> {
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
        if restart {
            self.stop()?;
        }
        if restart || selected || destination == Destination::Telegram || settings.targets.is_some() {
            self.write_settings(&settings)?;
        }
        // Restart unrelated targets even when the relay cannot revoke this one.
        let restarted = if restart { self.start_if_active(&settings) } else { Ok(()) };
        self.revoke_saved(destination, None).and_then(|()| self.discard(destination)).and(restarted)?;
        Ok(format!("Removed the saved {} destination and its secret.", destination.title()))
    }

    /// Sends one message Milo wrote itself to the selected destination.
    pub(crate) fn test(&self) -> Result<String, String> {
        let settings = self.editable()?;
        let destination = settings.destination.ok_or(NO_DESTINATION)?;
        self.send_test(&settings, destination)
    }

    /// Tests a saved provider whether or not it is selected or active.
    pub(crate) fn test_destination(&self, destination: Destination) -> Result<String, String> {
        self.send_test(&self.editable()?, destination)
    }

    fn send_test(&self, settings: &Settings, destination: Destination) -> Result<String, String> {
        let secret = self.target_secret(settings, destination)?;
        let request = self
            .message_request(destination, &secret, &settings.telegram_chat, message::TEST, None)
            .ok_or_else(|| format!("The saved {} secret is invalid. Save it again.", destination.title()))?;
        match send::post(&request, None) {
            send::Outcome::Sent => {
                Ok(format!("{} accepted the test message.", describe_destination(settings, destination)))
            }
            outcome => {
                let hosted = destination == Destination::Telegram && relay::valid_capability(&secret);
                Err(format!("The test message was not delivered: {}.", outcome.reason(destination, hosted)))
            }
        }
    }

    fn state_dir(&self) -> &Path {
        &self.state
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
