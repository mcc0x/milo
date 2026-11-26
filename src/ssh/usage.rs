//! Usage beside a shell that `milo ssh` may take to another computer.
//!
//! Usage reads accounts and tools of the computer it runs on, so it never
//! moves on its own: while the shell is on another host, it says it is this
//! computer's and offers that host's on Tab. The host's is Milo on the host
//! reading its own files, in the lent terminal; Tab or q there comes back.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use super::link::Link;
use crate::help::Page;
use crate::keys::Key;
use crate::pane::{After, External, Pane};
use crate::screen::{self, Screen, Style};
use crate::watch::Scope;

/// How often the shell's link is looked for. It is one small private file.
const LOOK: Duration = Duration::from_secs(1);
const HERE: &str = "This computer's usage";

#[derive(Debug, PartialEq, Eq)]
enum Shown {
    Here,
    /// The person chose the host's usage; it opens at the next turn of the loop.
    Chosen,
    Away,
    /// The host's usage ended without the person leaving it.
    Ended,
}

pub struct Scoped {
    usage: Box<dyn Pane>,
    anchor: String,
    state: Option<PathBuf>,
    link: Option<Link>,
    shown: Shown,
    looked: Instant,
    notice_only: bool,
}

impl Scoped {
    pub fn new(usage: Box<dyn Pane>, anchor: String, state: Option<PathBuf>) -> Self {
        let mut scoped =
            Self { usage, anchor, state, link: None, shown: Shown::Here, looked: Instant::now(), notice_only: false };
        scoped.look();
        scoped
    }

    fn look(&mut self) {
        self.looked = Instant::now();
        let found = self.state.as_deref().and_then(|state| Link::held(state, &self.anchor));
        if found.as_ref().map(|link| &link.token) != self.link.as_ref().map(|link| &link.token) {
            self.shown = Shown::Here;
        }
        self.link = found;
    }

    /// What the foot of the pane says, in parts: plain, the host, plain.
    fn foot(&self) -> Option<[&str; 3]> {
        let link = self.link.as_ref()?;
        Some(match (&self.shown, &link.helper) {
            (Shown::Ended, _) => [HERE, &link.host, "'s usage ended"],
            (_, Some(_)) => [HERE, "Tab: ", &link.host],
            (_, None) => [HERE, &link.host, " has no Milo"],
        })
    }
}

impl Pane for Scoped {
    fn draw(&mut self, screen: &mut Screen) {
        if self.looked.elapsed() >= LOOK {
            self.look();
        }
        let Some(foot) = self.foot().filter(|_| screen.h > 0).map(|parts| parts.map(str::to_owned)) else {
            self.notice_only = false;
            self.usage.draw(screen);
            return;
        };
        self.notice_only = screen.h == 1;
        screen.h -= 1;
        if self.notice_only {
            screen.clear();
        } else {
            self.usage.draw(screen);
        }
        screen.h += 1;
        draw_foot(screen, foot.each_ref().map(String::as_str));
    }

    fn help(&self) -> Page {
        self.usage.help()
    }

    fn notice(&self) -> Option<&'static str> {
        self.usage.notice()
    }

    fn watch_scope(&self) -> Scope {
        self.usage.watch_scope()
    }

    fn watch_scope_after_input(&self, current: &Scope) -> Option<Scope> {
        self.usage.watch_scope_after_input(current)
    }

    fn check(&mut self) -> bool {
        self.usage.check()
    }

    fn check_with_watcher(&mut self, watching: bool, started: Instant) -> bool {
        self.usage.check_with_watcher(watching, started)
    }

    fn unfinished_check(&self) -> Option<std::os::fd::RawFd> {
        self.usage.unfinished_check()
    }

    fn finish_check(&mut self) -> Option<bool> {
        self.usage.finish_check()
    }

    fn filesystem_changed(&mut self) {
        self.usage.filesystem_changed();
    }

    fn external_fd(&self) -> Option<std::os::fd::RawFd> {
        self.usage.external_fd()
    }

    fn check_external(&mut self) -> External {
        self.usage.check_external()
    }

    fn next_frame(&self) -> Option<Duration> {
        let look = LOOK.saturating_sub(self.looked.elapsed());
        Some(self.usage.next_frame().map_or(look, |inner| inner.min(look)))
    }

    fn key(&mut self, key: Key) -> After {
        if key == Key::Ctrl(b'i') && self.link.as_ref().is_some_and(|link| link.helper.is_some()) {
            self.shown = Shown::Chosen;
            return After::Stay;
        }
        if self.notice_only && !key.while_obscured() {
            return After::Ignored;
        }
        self.usage.key(key)
    }

    fn lend(&mut self) -> Option<Command> {
        if self.shown != Shown::Chosen {
            return None;
        }
        let command = super::pane_command(self.link.as_ref()?, "usage")?;
        self.shown = Shown::Away;
        Some(command)
    }

    /// The host's usage stays only while its `milo ssh` holds the connection.
    fn still_lent(&mut self) -> bool {
        self.state.as_deref().is_some_and(|state| self.link.as_ref().is_some_and(|link| link.current(state, &self.anchor)))
    }

    fn returned(&mut self, finished: bool) -> After {
        let held = self.still_lent();
        self.shown = if finished || !held { Shown::Here } else { Shown::Ended };
        self.look();
        After::Stay
    }
}

/// Usage on the host, shown in a terminal lent from another computer.
pub struct Labeled {
    usage: Box<dyn Pane>,
    host: String,
    notice_only: bool,
}

impl Labeled {
    pub fn new(usage: Box<dyn Pane>, host: &str) -> Self {
        Self { usage, host: host.to_owned(), notice_only: false }
    }
}

impl Pane for Labeled {
    fn draw(&mut self, screen: &mut Screen) {
        if screen.h == 0 {
            return;
        }
        self.notice_only = screen.h == 1;
        screen.h -= 1;
        if self.notice_only {
            screen.clear();
        } else {
            self.usage.draw(screen);
        }
        screen.h += 1;
        draw_foot(screen, ["", &self.host, "'s usage · Tab: back to the other computer's"]);
    }

    fn help(&self) -> Page {
        self.usage.help()
    }

    fn notice(&self) -> Option<&'static str> {
        self.usage.notice()
    }

    fn watch_scope(&self) -> Scope {
        self.usage.watch_scope()
    }

    fn watch_scope_after_input(&self, current: &Scope) -> Option<Scope> {
        self.usage.watch_scope_after_input(current)
    }

    fn check(&mut self) -> bool {
        self.usage.check()
    }

    fn check_with_watcher(&mut self, watching: bool, started: Instant) -> bool {
        self.usage.check_with_watcher(watching, started)
    }

    fn unfinished_check(&self) -> Option<std::os::fd::RawFd> {
        self.usage.unfinished_check()
    }

    fn finish_check(&mut self) -> Option<bool> {
        self.usage.finish_check()
    }

    fn filesystem_changed(&mut self) {
        self.usage.filesystem_changed();
    }

    fn external_fd(&self) -> Option<std::os::fd::RawFd> {
        self.usage.external_fd()
    }

    fn check_external(&mut self) -> External {
        self.usage.check_external()
    }

    fn next_frame(&self) -> Option<Duration> {
        self.usage.next_frame()
    }

    fn key(&mut self, key: Key) -> After {
        if key == Key::Ctrl(b'i') {
            return After::Leave;
        }
        if self.notice_only && !key.while_obscured() {
            return After::Ignored;
        }
        self.usage.key(key)
    }
}

/// Draws a foot row: plain words, the host in its own colour, plain words.
fn draw_foot(screen: &mut Screen, [before, host, after]: [&str; 3]) {
    let (width, y, plain) = (screen.w.saturating_sub(2), screen.h - 1, Style::fg(screen::YELLOW));
    let mut used = 0;
    if !before.is_empty() {
        used += screen.put_fit(1, y, width, before, plain);
        used += screen.put(1 + used, y, width.saturating_sub(used), " · ", plain);
    }
    for (text, style) in [(host, Style::fg(screen::CYAN)), (after, plain)] {
        used += screen.put_fit(1 + used, y, width.saturating_sub(used), text, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use crate::ssh::process::Process;

    struct Plain;
    impl Pane for Plain {
        fn draw(&mut self, screen: &mut Screen) {
            screen.clear();
            screen.put(0, 0, screen.w, "Claude 5h 40%", Style::fg(screen::YELLOW));
        }
        fn help(&self) -> Page {
            Page::new(vec![])
        }
        fn key(&mut self, key: Key) -> After {
            if key == Key::Char('q') { After::Leave } else { After::Ignored }
        }
    }

    fn link(helper: Option<&str>) -> Link {
        Link {
            owner: Process::current().unwrap(),
            token: "c".repeat(32),
            host: "ub-1".into(),
            control: PathBuf::from("/tmp/milo-ssh-c/c"),
            config: None,
            helper: helper.map(str::to_owned),
        }
    }

    fn text(pane: &mut dyn Pane) -> Vec<String> {
        let mut screen = Screen::sized(60, 3);
        pane.draw(&mut screen);
        screen.text()
    }

    #[test]
    fn usage_stays_on_this_computer_until_the_host_s_is_chosen() {
        let state = Fixture::folder();
        let mut pane = Scoped::new(Box::new(Plain), "block:1".into(), Some(state.root.clone()));
        assert_eq!(text(&mut pane)[0], "Claude 5h 40%");
        assert!(text(&mut pane)[2].trim().is_empty(), "no foot without a link");
        assert!(pane.lend().is_none());
        assert_eq!(pane.key(Key::Ctrl(b'i')), After::Ignored);

        let ours = link(Some("/opt/milo"));
        ours.write(&state.root, "block:1").unwrap();
        pane.look();
        assert_eq!(text(&mut pane)[2].trim(), "This computer's usage · Tab: ub-1");
        assert!(pane.lend().is_none(), "nothing moves until the person chooses");
        assert_eq!(pane.key(Key::Ctrl(b'i')), After::Stay);
        let lent = pane.lend().unwrap();
        assert!(lent.get_args().any(|arg| arg == "usage"));
        assert_eq!(pane.returned(true), After::Stay);
        assert_eq!(text(&mut pane)[2].trim(), "This computer's usage · Tab: ub-1");
        assert_eq!(pane.key(Key::Ctrl(b'i')), After::Stay);
        pane.lend().unwrap();
        assert_eq!(pane.returned(false), After::Stay);
        assert_eq!(text(&mut pane)[2].trim(), "This computer's usage · ub-1's usage ended");

        ours.remove(&state.root, "block:1");
        Link { token: "d".repeat(32), ..link(None) }.write(&state.root, "block:1").unwrap();
        pane.look();
        assert_eq!(text(&mut pane)[2].trim(), "This computer's usage · ub-1 has no Milo");
        assert_eq!(pane.key(Key::Ctrl(b'i')), After::Ignored);
        assert_eq!(pane.key(Key::Char('q')), After::Leave);
    }

    #[test]
    fn the_host_s_usage_names_the_host_and_tab_goes_back() {
        let mut pane = Labeled::new(Box::new(Plain), "ub-1");
        assert_eq!(text(&mut pane)[2].trim(), "ub-1's usage · Tab: back to the other computer's");
        assert_eq!(pane.key(Key::Ctrl(b'i')), After::Leave);
    }
}
