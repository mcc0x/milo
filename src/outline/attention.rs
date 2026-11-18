//! Decides when the outline may follow work and when its marks have been seen.

use std::time::{Duration, Instant};

/// How long an outline without focus reports waits before following work.
const FOLLOW_UNTOLD: Duration = Duration::from_secs(120);
/// How long a mark stays after it has been seen.
const FADE: Duration = Duration::from_secs(4);
/// How long a mark can wait to be seen.
const UNSEEN: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Unknown,
    Here,
    Elsewhere,
}

pub(super) struct Attention {
    focus: Focus,
    used: Instant,
}

impl Attention {
    pub(super) fn new(now: Instant) -> Self {
        Self { focus: Focus::Unknown, used: now }
    }

    pub(super) fn used(&mut self, now: Instant) {
        self.used = now;
    }

    pub(super) fn focus(&mut self, here: bool, now: Instant) {
        self.focus = if here { Focus::Here } else { Focus::Elsewhere };
        if here {
            self.used(now);
        }
    }

    pub(super) fn following(&self, now: Instant) -> bool {
        match self.focus {
            Focus::Here => false,
            Focus::Elsewhere => true,
            Focus::Unknown => now.saturating_duration_since(self.used) >= FOLLOW_UNTOLD,
        }
    }

    /// Keeps the cursor bar and key hint visible unless focus is known to be elsewhere.
    pub(super) fn show_controls(&self) -> bool {
        self.focus != Focus::Elsewhere
    }

    /// A mark is seen on arrival when focused, or on the next use of the pane.
    pub(super) fn mark_left(&self, at: Instant, now: Instant) -> Duration {
        let seen = match self.focus {
            Focus::Here => Some(at.max(self.used)),
            _ if self.used > at => Some(self.used),
            _ => None,
        };
        let left = |from, limit: Duration| limit.saturating_sub(now.saturating_duration_since(from));
        seen.map_or(UNSEEN, |from| left(from, FADE)).min(left(at, UNSEEN))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_focus_controls_following_and_visibility() {
        let now = Instant::now();
        let mut attention = Attention::new(now);
        attention.focus(true, now);
        assert!(!attention.following(now + Duration::from_secs(600)));
        assert!(attention.show_controls());
        attention.focus(false, now);
        assert!(attention.following(now));
        assert!(!attention.show_controls());
    }

    #[test]
    fn without_a_focus_report_following_starts_after_two_minutes_of_silence() {
        let now = Instant::now();
        let mut attention = Attention::new(now);
        assert!(attention.show_controls());
        assert!(!attention.following(now + Duration::from_secs(119)));
        let later = now + Duration::from_secs(120);
        assert!(attention.following(later));
        attention.used(later);
        assert!(!attention.following(later));
    }

    #[test]
    fn a_mark_fades_four_seconds_after_it_is_seen() {
        let now = Instant::now();
        let at = now + Duration::from_secs(1);
        let mut attention = Attention::new(now);
        attention.focus(true, now);
        assert_eq!(attention.mark_left(at, at), Duration::from_secs(4));
        assert_eq!(attention.mark_left(at, at + Duration::from_secs(4)), Duration::ZERO);

        attention.focus(false, now);
        let seen = at + Duration::from_secs(30);
        attention.focus(true, seen);
        assert_eq!(attention.mark_left(at, seen), Duration::from_secs(4));
        assert_eq!(attention.mark_left(at, seen + Duration::from_secs(4)), Duration::ZERO);
    }

    #[test]
    fn an_unseen_mark_is_dropped_after_five_minutes() {
        let now = Instant::now();
        let at = now + Duration::from_secs(1);
        let mut attention = Attention::new(now);
        assert_eq!(attention.mark_left(at, at + Duration::from_secs(299)), Duration::from_secs(1));
        assert_eq!(attention.mark_left(at, at + Duration::from_secs(300)), Duration::ZERO);
        attention.used(at + Duration::from_secs(298));
        assert_eq!(attention.mark_left(at, at + Duration::from_secs(299)), Duration::from_secs(1));
    }
}
