//! Uses keyboard focus to decide whether to follow work and when to fade marks.

use std::collections::BTreeMap;
use std::ops::Bound::{Included, Unbounded};
use std::time::{Duration, Instant};

/// How long an outline without focus reports waits before following work.
const FOLLOW_UNTOLD: Duration = Duration::from_secs(120);
/// Marks pulse for three seconds after a change.
pub(super) const LIVE: Duration = Duration::from_secs(3);
pub(super) const PULSE: Duration = Duration::from_millis(500);
/// How long a mark stays after it has been seen.
const FADE: Duration = Duration::from_secs(4);
/// How long a mark can stay visible even if the user never sees it.
const UNSEEN: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Unknown,
    Here,
    Elsewhere,
}

struct Mark {
    at: Instant,
    until: Instant,
}

pub(super) struct Attention {
    focus: Focus,
    used: Instant,
    marks: BTreeMap<String, Mark>,
    unseen_marks: bool,
}

impl Attention {
    pub(super) fn new(now: Instant) -> Self {
        Self { focus: Focus::Unknown, used: now, marks: BTreeMap::new(), unseen_marks: false }
    }

    /// Returns whether mark expiry changed, so the next frame can be scheduled again.
    pub(super) fn used(&mut self, now: Instant) -> bool {
        self.used = now;
        let unseen = std::mem::take(&mut self.unseen_marks);
        if unseen {
            for mark in self.marks.values_mut() {
                mark.until = mark.until.min(now + FADE);
            }
        }
        unseen
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
    pub(super) fn mark(&mut self, path: &str, now: Instant) {
        let here = self.focus == Focus::Here;
        self.unseen_marks |= !here;
        let mark = Mark { at: now, until: now + if here { FADE } else { UNSEEN } };
        if let Some(old) = self.marks.get_mut(path) {
            *old = mark;
        } else {
            self.marks.insert(path.to_owned(), mark);
        }
    }

    pub(super) fn expire_marks(&mut self, now: Instant) {
        self.marks.retain(|_, mark| mark.until > now);
    }

    pub(super) fn next_mark_frame(&self, now: Instant, static_marks: bool) -> Option<Duration> {
        self.marks
            .values()
            .map(|mark| {
                let left = mark.until.saturating_duration_since(now);
                if !static_marks && now.saturating_duration_since(mark.at) < LIVE { left.min(PULSE) } else { left }
            })
            .min()
    }

    /// A closed folder path ends in `/` and carries the latest mark among the files it hides.
    pub(super) fn marked_at(&self, path: &str, closed_folder: bool) -> Option<Instant> {
        if closed_folder {
            self.marks
                .range::<str, _>((Included(path), Unbounded))
                .take_while(|(file, _)| file.starts_with(path))
                .map(|(_, mark)| mark.at)
                .max()
        } else {
            self.marks.get(path).map(|mark| mark.at)
        }
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
    fn a_seen_mark_fades_even_if_focus_moves_or_the_user_keeps_typing() {
        let now = Instant::now();
        let at = now + Duration::from_secs(1);
        let mut attention = Attention::new(now);
        attention.focus(true, now);
        attention.mark("focused", at);
        attention.focus(false, at + Duration::from_secs(1));
        attention.expire_marks(at + Duration::from_secs(3));
        assert_eq!(attention.marked_at("focused", false), Some(at));
        attention.expire_marks(at + Duration::from_secs(4));
        assert_eq!(attention.marked_at("focused", false), None);

        let at = at + Duration::from_secs(5);
        attention.mark("unfocused", at);
        let seen = at + Duration::from_secs(30);
        attention.focus(true, seen);
        attention.used(seen + Duration::from_secs(2));
        attention.expire_marks(seen + Duration::from_secs(3));
        assert_eq!(attention.marked_at("unfocused", false), Some(at));
        attention.expire_marks(seen + Duration::from_secs(4));
        assert_eq!(attention.marked_at("unfocused", false), None);
    }

    #[test]
    fn an_unseen_mark_is_dropped_after_five_minutes() {
        let at = Instant::now();
        let mut attention = Attention::new(at);
        attention.mark("a", at);
        let almost = at + Duration::from_secs(299);
        assert_eq!(attention.next_mark_frame(at, false), Some(PULSE));
        assert_eq!(attention.next_mark_frame(at, true), Some(UNSEEN));
        assert_eq!(attention.next_mark_frame(almost, false), Some(Duration::from_secs(1)));
        attention.used(at + Duration::from_secs(298));
        assert_eq!(attention.next_mark_frame(almost, false), Some(Duration::from_secs(1)));
        attention.expire_marks(at + Duration::from_secs(300));
        assert_eq!(attention.marked_at("a", false), None);
    }
}
