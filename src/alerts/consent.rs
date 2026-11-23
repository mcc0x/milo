//! Captures what a confirmation showed without exposing the saved credentials.

use std::fmt::{self, Write as _};
use std::hash::{BuildHasher as _, RandomState};
use std::sync::LazyLock;

use super::{Alerts, Destination, Kind, Settings, UNREADABLE, describe_destination};

// A process-local key keeps this an opaque comparison tag, not a public
// fingerprint of a credential. Two domains give a 128-bit tag.
static KEY: LazyLock<RandomState> = LazyLock::new(RandomState::new);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConsentToken([u64; 2]);

impl ConsentToken {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        if text.len() != 32 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let (first, second) = text.split_at(16);
        Some(Self([u64::from_str_radix(first, 16).ok()?, u64::from_str_radix(second, 16).ok()?]))
    }
}

impl fmt::Display for ConsentToken {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self([first, second]) = self;
        write!(out, "{first:016x}{second:016x}")
    }
}

const STALE: &str =
    "Alert destinations or what gets sent changed. Review them again before enabling forwarding or notices.";

#[derive(Clone, Copy)]
pub(crate) enum ConsentFor {
    All,
    Destination(Destination),
}

impl ConsentFor {
    fn targets(self, settings: &Settings) -> [bool; 2] {
        match self {
            Self::All => settings.enable_targets(),
            Self::Destination(target) => Destination::ALL.map(|destination| destination == target),
        }
    }

    fn token(self, settings: &Settings, credentials: [Option<&str>; 2], notices: [bool; 2]) -> Option<ConsentToken> {
        let targets = self.targets(settings);
        if !targets.contains(&true) {
            return None;
        }
        let [telegram, discord] = targets;
        let [telegram_secret, discord_secret] = credentials;
        let [telegram_notices, discord_notices] = notices;
        let [telegram_revision, discord_revision] = settings.connection_revision;
        let telegram = if telegram {
            Some((settings.telegram_chat.as_str(), telegram_secret?, telegram_notices, telegram_revision))
        } else {
            None
        };
        let discord = if discord { Some((discord_secret?, discord_notices, discord_revision)) } else { None };
        let scope = match self {
            Self::All => "all",
            Self::Destination(destination) => destination.name(),
        };
        let reviewed = (
            scope,
            telegram,
            discord,
            settings.kinds,
            settings.content.name(),
            settings.origin,
            settings.disclosure_revision,
        );
        Some(ConsentToken([0u8, 1].map(|domain| KEY.hash_one((domain, reviewed)))))
    }

    fn check(self, settings: &Settings, credentials: [Option<&str>; 2], expected: ConsentToken) -> Result<(), String> {
        if self.token(settings, credentials, settings.bound_notices(credentials)) == Some(expected) {
            Ok(())
        } else {
            Err(STALE.into())
        }
    }
}

/// One snapshot for each provider switch, and one for resuming the configured targets together.
pub(crate) struct Consent {
    all: Option<ConsentToken>,
    destinations: [Option<ConsentToken>; 2],
}

impl Consent {
    pub(super) fn at(settings: &Settings, credentials: [Option<&str>; 2]) -> Self {
        Self {
            all: ConsentFor::All.token(settings, credentials, settings.notices),
            destinations: Destination::ALL
                .map(|destination| ConsentFor::Destination(destination).token(settings, credentials, settings.notices)),
        }
    }

    pub(crate) fn token(&self, scope: ConsentFor) -> Option<ConsentToken> {
        match scope {
            ConsentFor::All => self.all,
            ConsentFor::Destination(destination) => self.destinations[destination as usize],
        }
    }
}

impl Alerts {
    /// Recheck the reviewed destination and disclosure in the write transaction, not in the caller.
    pub(crate) fn enable_reviewed(&self, scope: ConsentFor, expected: ConsentToken) -> Result<String, String> {
        self.locked(|edit| {
            let settings = edit.settings().map_err(|_| STALE.to_owned())?;
            let targets = scope.targets(&settings);
            let credentials = Destination::ALL.map(|destination| {
                if targets[destination as usize] { edit.saved_secret(destination).ok().flatten() } else { None }
            });
            let credentials = credentials.each_ref().map(|secret| secret.as_deref());
            scope.check(&settings, credentials, expected)?;
            match scope {
                ConsentFor::All => edit.enable_with(settings, Some(credentials)),
                ConsentFor::Destination(destination) => {
                    edit.set_forwarding_with(settings, destination, true, Some(credentials))
                }
            }
        })
    }

    /// A confirmation for a previous or less private connection cannot opt in a newer one.
    pub(crate) fn set_notices_reviewed(
        &self,
        destination: Destination,
        expected: ConsentToken,
    ) -> Result<String, String> {
        self.locked(|edit| {
            let settings = edit.settings().map_err(|_| STALE.to_owned())?;
            let secret = edit.saved_secret(destination).ok().flatten();
            let mut credentials = [None; 2];
            credentials[destination as usize] = secret.as_deref();
            ConsentFor::Destination(destination).check(&settings, credentials, expected)?;
            edit.set_notices_with(settings, destination, true, Some(credentials))
        })
    }

    /// Selecting a legacy destination in Settings must not reroute active forwarding without review.
    pub(crate) fn choose_for_review(&self, destination: Destination) -> Result<String, String> {
        self.locked(|edit| {
            let settings = edit.settings().map_err(|_| UNREADABLE.to_owned())?;
            if settings.enabled && settings.targets.is_none() && settings.destination != Some(destination) {
                return Err("Turn mirroring off before changing the selected destination.".into());
            }
            edit.choose(destination)
        })
    }
}

impl Settings {
    pub(crate) fn forwarding_review(&self) -> Option<String> {
        let targets = self.enable_targets();
        let mut destinations = Destination::ALL.into_iter().filter(|&destination| targets[destination as usize]);
        let first = destinations.next()?;
        let mut review = format!("Destination: {}", describe_destination(self, first));
        for destination in destinations {
            let _ = write!(review, " and {}", describe_destination(self, destination));
        }
        review.push_str(".\nEvents: ");
        let mut kinds = Kind::ALL.into_iter().filter(|&kind| self.mirrors(kind));
        review.push_str(kinds.next().map_or("none", Kind::name));
        for kind in kinds {
            review.push_str(", ");
            review.push_str(kind.name());
        }
        review.push('.');
        let mut notices =
            Destination::ALL.into_iter().filter(|&target| targets[target as usize] && self.notices[target as usize]);
        if let Some(first) = notices.next() {
            let _ = write!(review, "\nLifecycle notices (forwarding status only): {}", first.title());
            for destination in notices {
                let _ = write!(review, ", {}", destination.title());
            }
            review.push('.');
        }
        Some(review)
    }
}
