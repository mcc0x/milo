//! Retargets a maintained pane to the worktree of its explicitly chosen shell,
//! and hands its terminal to the same pane on another computer while that
//! shell is there through `milo ssh`.

use std::ffi::OsStr;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::git::Repo;
use crate::help::Page;
use crate::keys::Key;
use crate::pane::{After, External, Pane};
use crate::preferences::{Preferences, ProjectMode};
use crate::rex::{ShellChange, ShellNews, ShellObserver};
use crate::screen::{self, Screen, Style};
use crate::ssh::helper::Role;
use crate::ssh::link::Link;
use crate::ssh::process::Process;
use crate::ssh::watch::ShellWatch;
use crate::watch::Scope;

pub fn project(
    repo: Repo,
    shell: Option<&OsStr>,
    role: Role,
    make: impl Fn(Repo, &Preferences) -> Box<dyn Pane> + 'static,
) -> Box<dyn Pane> {
    let (prefs, preference_note) = panel_preferences(&repo);
    let shell = shell.filter(|_| prefs.project_mode == ProjectMode::Follow);
    if shell.is_none() && preference_note.is_none() {
        return make(repo, &prefs);
    }
    let source = shell.and_then(ShellObserver::start).map(Source::Rex);
    let tab = shell.zip(crate::paths::state_dir()).zip(source.as_ref()).and_then(|((anchor, state), _)| {
        Some(Tab { anchor: anchor.to_str()?.to_owned(), state, role, foreground: Vec::new(), remote: Remote::Local })
    });
    Box::new(Project {
        root: repo.root.clone(),
        git: repo.git,
        pane: make(repo, &prefs),
        make,
        note: (shell.is_some() && source.is_none()).then_some(NO_OBSERVER),
        notice_only: false,
        preference_note,
        source,
        last_dir: None,
        retry: None,
        focus: None,
        tab,
        host: None,
    })
}

/// A pane on this computer that follows `shell`, which `milo ssh` started
/// here from a tab on another computer. `host` is this computer as the person
/// named it there, and is shown on the pane.
pub fn remote(
    repo: Repo,
    shell: Process,
    host: String,
    make: impl Fn(Repo, &Preferences) -> Box<dyn Pane> + 'static,
) -> Box<dyn Pane> {
    let (prefs, preference_note) = panel_preferences(&repo);
    let source = ShellWatch::start(shell).ok().map(Source::Host);
    Box::new(Project {
        root: repo.root.clone(),
        git: repo.git,
        pane: make(repo, &prefs),
        make,
        note: source.is_none().then_some(NO_OBSERVER),
        notice_only: false,
        preference_note,
        source,
        last_dir: None,
        retry: None,
        focus: None,
        tab: None,
        host: Some(host),
    })
}

fn panel_preferences(repo: &Repo) -> (Preferences, Option<&'static str>) {
    Preferences::for_pane(repo.git.then_some(repo.root.as_path()))
}

const NO_CWD: &str = "Waiting for shell";
const NO_OBSERVER: &str = "Shell tracking unavailable";
const BAD_CWD: &str = "Shell folder unavailable";
const DISCOVERY_FAILED: &str = "Could not identify shell project";
const CONNECTING: &str = "Opening";
const UNFOLLOWED: &str = "Can't follow there";
const ENDED: &str = "Its pane ended";

/// Where the shell's folder comes from: Rex on this computer, or this
/// computer's system for a shell that `milo ssh` started here.
enum Source {
    Rex(ShellObserver),
    Host(ShellWatch),
}

impl Source {
    fn fd(&self) -> RawFd {
        match self {
            Self::Rex(observer) => observer.fd(),
            Self::Host(watch) => watch.fd(),
        }
    }

    fn read(&mut self) -> ShellNews {
        match self {
            Self::Rex(observer) => observer.read(),
            Self::Host(watch) => watch.read(),
        }
    }
}

/// The Rex tab whose shell this pane follows, and where that shell is now.
struct Tab {
    /// The shell's Rex block.
    anchor: String,
    state: PathBuf,
    role: Role,
    /// The processes in front of the shell, as Rex last reported them.
    foreground: Vec<i32>,
    remote: Remote,
}

#[derive(Debug)]
enum Remote {
    Local,
    /// The shell is on the link's host; the pane there opens at the next turn of the loop.
    Due(Link),
    /// The terminal is lent to the pane on the link's host.
    Away(Link),
    /// The shell is on the link's host, which has no Milo to show it.
    Unfollowed(Link),
    /// The pane on the link's host ended while the shell is still there.
    Ended(Link),
}

impl Remote {
    fn link(&self) -> Option<&Link> {
        match self {
            Self::Local => None,
            Self::Due(link) | Self::Away(link) | Self::Unfollowed(link) | Self::Ended(link) => Some(link),
        }
    }

    fn note(&self) -> Option<&'static str> {
        match self {
            Self::Local | Self::Away(_) => None,
            Self::Due(_) => Some(CONNECTING),
            Self::Unfollowed(_) => Some(UNFOLLOWED),
            Self::Ended(_) => Some(ENDED),
        }
    }
}

struct Project<F> {
    root: PathBuf,
    git: bool,
    pane: Box<dyn Pane>,
    make: F,
    source: Option<Source>,
    last_dir: Option<PathBuf>,
    retry: Option<(PathBuf, Instant)>,
    note: Option<&'static str>,
    notice_only: bool,
    preference_note: Option<&'static str>,
    focus: Option<bool>,
    tab: Option<Tab>,
    /// This computer's name, as shown on a pane that follows a shell here from another one.
    host: Option<String>,
}

impl<F: Fn(Repo, &Preferences) -> Box<dyn Pane>> Project<F> {
    fn observe(&mut self, dir: PathBuf) -> External {
        self.retry = None;
        if self.last_dir.as_ref() == Some(&dir) {
            return self.set_note(None);
        }
        let Some(repo) = Repo::open(&dir) else {
            return self.set_note(Some(BAD_CWD));
        };
        // A failed discovery inside the current checkout isn't evidence that
        // the shell left it. Rex reports cwd changes, not Git availability.
        if repo.git_unavailable()
            || (!repo.git && self.git && repo.root.starts_with(&self.root) && self.root.join(".git").exists())
        {
            self.retry = Some((dir, Instant::now() + Duration::from_secs(5)));
            return self.set_note(Some(DISCOVERY_FAILED));
        }
        self.last_dir = Some(dir);
        if self.root == repo.root && self.git == repo.git {
            return self.set_note(None);
        }
        let (prefs, preference_note) = panel_preferences(&repo);
        self.preference_note = preference_note;
        self.root.clone_from(&repo.root);
        self.git = repo.git;
        let mut pane = (self.make)(repo, &prefs);
        if let (Some(previous), Some(next)) = (self.pane.reader_opener(), pane.reader_opener()) {
            std::mem::swap(previous, next);
        }
        self.pane = pane;
        if let Some(focused) = self.focus {
            self.pane.key(Key::Focus(focused));
        }
        if prefs.project_mode == ProjectMode::Pin {
            self.source = None;
            self.tab = None;
        }
        self.note = None;
        External::NewScope
    }

    /// Takes in which program is in front of the shell, and so whether a
    /// `milo ssh` there has taken it to another computer or brought it back.
    fn look_remote(&mut self, foreground: Vec<i32>) -> External {
        let Some(tab) = &mut self.tab else { return External::Unchanged };
        tab.foreground = foreground;
        if matches!(tab.remote, Remote::Away(_)) {
            return External::Unchanged;
        }
        let found = Link::find(&tab.state, &tab.anchor, &tab.foreground);
        if found.as_ref().map(|link| &link.token) == tab.remote.link().map(|link| &link.token) {
            return External::Unchanged;
        }
        tab.remote = match found {
            None => Remote::Local,
            Some(link) if link.helper.is_some() => Remote::Due(link),
            Some(link) => Remote::Unfollowed(link),
        };
        External::Redraw
    }

    fn sync_promotion(&mut self) {
        if self.git {
            return;
        }
        let Some((root, mode)) = self.pane.promoted_worktree() else { return };
        if self.root != root {
            root.clone_into(&mut self.root);
        }
        self.git = true;
        // The inner pane now owns the notice from the newly loaded preferences.
        self.preference_note = None;
        if mode == ProjectMode::Pin {
            self.source = None;
            self.tab = None;
            self.note = None;
        }
    }

    fn set_note(&mut self, note: Option<&'static str>) -> External {
        if std::mem::replace(&mut self.note, note) == note { External::Unchanged } else { External::Redraw }
    }

    /// The host named at the foot of the pane: this one's, or the one its shell went to.
    fn foot_host(&self) -> Option<&str> {
        self.host.as_deref().or_else(|| Some(self.tab.as_ref()?.remote.link()?.host.as_str()))
    }

    fn foot_note(&self) -> Option<&'static str> {
        self.tab.as_ref().and_then(|tab| tab.remote.note()).or(self.note)
    }
}

impl<F: Fn(Repo, &Preferences) -> Box<dyn Pane>> Pane for Project<F> {
    fn draw(&mut self, screen: &mut Screen) {
        let (host, note) = (self.foot_host().map(str::to_owned), self.foot_note());
        self.notice_only = (host.is_some() || note.is_some()) && screen.h == 1;
        if host.is_none() && note.is_none() || screen.h == 0 {
            self.pane.draw(screen);
            return;
        }
        // The pane lays out only rows it can really draw into, so the notice
        // doesn't cover a selectable row or jump on top of a picture.
        screen.h -= 1;
        if self.notice_only {
            screen.clear();
        } else {
            self.pane.draw(screen);
        }
        screen.h += 1;
        let (width, y, yellow) = (screen.w.saturating_sub(2), screen.h - 1, Style::fg(screen::YELLOW));
        const SEPARATOR: &str = " · ";
        let mut used = 0;
        for (text, style) in [(host.as_deref(), Style::fg(screen::CYAN)), (note, yellow)] {
            let Some(text) = text else { continue };
            if used > 0 {
                used += screen.put(1 + used, y, width - used, SEPARATOR, style);
            }
            used += screen.put_fit(1 + used, y, width - used, text, style);
        }
        const SHOWING: &str = " · showing ";
        if let Some(project) = self.root.file_name().and_then(OsStr::to_str)
            && width > used + crate::width::str_width(SHOWING) + 2
        {
            let start = used + screen.put(1 + used, y, width - used, SHOWING, yellow);
            screen.put_fit(1 + start, y, width - start, project, yellow);
        }
    }
    fn notice(&self) -> Option<&'static str> {
        self.preference_note.or_else(|| self.pane.notice())
    }

    fn tracks_hover(&self) -> bool {
        !self.notice_only && self.pane.tracks_hover()
    }

    fn hover_span(&self, x: usize, y: usize) -> Option<std::ops::Range<usize>> {
        if self.notice_only { None } else { self.pane.hover_span(x, y) }
    }

    fn help(&self) -> Page {
        self.pane.help()
    }

    fn cover(&mut self) {
        self.pane.cover();
    }

    fn watch_scope(&self) -> Scope {
        self.pane.watch_scope()
    }

    fn watch_scope_after_input(&self, current: &Scope) -> Option<Scope> {
        self.pane.watch_scope_after_input(current)
    }

    fn git_busy(&self) -> bool {
        self.pane.git_busy()
    }

    fn check(&mut self) -> bool {
        if self.retry.as_ref().is_some_and(|(_, due)| Instant::now() >= *due)
            && let Some((dir, _)) = self.retry.take()
        {
            self.observe(dir);
        }
        let settled = self.pane.check();
        self.sync_promotion();
        settled
    }

    fn unfinished_check(&self) -> Option<RawFd> {
        self.pane.unfinished_check()
    }

    fn finish_check(&mut self) -> Option<bool> {
        self.pane.finish_check()
    }

    fn filesystem_changed(&mut self) {
        self.pane.filesystem_changed();
    }

    fn external_fd(&self) -> Option<RawFd> {
        self.source.as_ref().map(Source::fd)
    }

    fn check_external(&mut self) -> External {
        // The cwd and filesystem notifications can become ready together. Let
        // an in-place promotion apply its preferences before following the cwd.
        let promoted = self.source.is_some() && !self.git && self.root.join(".git").exists() && {
            self.check();
            self.git
        };
        let news = self.source.as_mut().map(Source::read);
        let (change, foreground) = news.map_or((ShellChange::Unchanged, None), |news| (news.change, news.foreground));
        let moved = foreground.map_or(External::Unchanged, |foreground| self.look_remote(foreground));
        let update = match change {
            ShellChange::Directory(dir) => self.observe(dir),
            ShellChange::Unreported => {
                self.retry = None;
                self.set_note(Some(NO_CWD))
            }
            ShellChange::Closed => {
                self.source = None;
                self.retry = None;
                self.set_note(Some(NO_OBSERVER))
            }
            ShellChange::Unchanged => External::Unchanged,
        };
        match (promoted, update, moved) {
            (true, ..) | (_, External::NewScope, _) => External::NewScope,
            (_, External::Redraw, _) | (_, _, External::Redraw) => External::Redraw,
            _ => External::Unchanged,
        }
    }

    fn lend(&mut self) -> Option<Command> {
        let tab = self.tab.as_mut()?;
        let Remote::Due(link) = &tab.remote else { return None };
        let command = crate::ssh::pane_command(link, tab.role.word())?;
        tab.remote = Remote::Away(link.clone());
        Some(command)
    }

    fn returned(&mut self, finished: bool) -> After {
        let Some(tab) = &mut self.tab else { return After::Stay };
        let Remote::Away(link) = std::mem::replace(&mut tab.remote, Remote::Local) else { return After::Stay };
        // A shell that logged out or lost its connection has taken its link with it.
        if !link.current(&tab.state, &tab.anchor) {
            return After::Stay;
        }
        // The person closed the pane on the host while still there, as q closes this one.
        if finished {
            return After::Leave;
        }
        tab.remote = Remote::Ended(link);
        After::Stay
    }

    fn next_frame(&self) -> Option<Duration> {
        if self.notice_only { None } else { self.pane.next_frame() }
    }

    fn editing(&self) -> bool {
        !self.notice_only && self.pane.editing()
    }

    fn paste(&mut self, text: Option<&str>) -> After {
        if self.notice_only { After::Ignored } else { self.pane.paste(text) }
    }

    fn key(&mut self, key: Key) -> After {
        // A one-row notice hides every choice, including the last painted menu.
        if self.notice_only && !key.while_obscured() {
            return After::Ignored;
        }
        if let Key::Focus(focused) = key {
            self.focus = Some(focused);
        }
        let after = self.pane.key(key);
        self.sync_promotion();
        after
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use crate::outline::Outline;
    use crate::reader::{Floated, OpenReader};
    use crate::ssh::link::Link;

    fn floated(block: &str) -> Floated {
        Floated { block: block.into(), root: PathBuf::new() }
    }

    #[test]
    fn retargets_both_pane_and_watcher_only_when_the_project_changes() {
        let first = Fixture::new();
        first.write("src/lib.rs", "first\n");
        first.commit("first");
        let second = Fixture::new();
        second.write("other.rs", "second\n");
        second.commit("second");
        let make =
            |repo, prefs: &Preferences| -> Box<dyn Pane> { Box::new(Outline::with_preferences(repo, true, prefs)) };
        let repo = Repo::open(&first.root).unwrap();
        let mut pane = Project {
            root: repo.root.clone(),
            git: true,
            pane: make(repo, &Preferences::default()),
            make,
            source: None,
            last_dir: None,
            retry: None,
            note: None,
            notice_only: false,
            preference_note: None,
            focus: Some(false),
            tab: None,
            host: None,
        };
        *pane.pane.reader_opener().unwrap() = OpenReader::Floating(Some(floated("block:previous")));
        assert!(matches!(pane.observe(first.root.join("src")), External::Unchanged));
        // Break only this fixture's Git discovery while the shell moves. The
        // previous worktree stays visible and recovers without a second cd.
        let config = first.root.join(".git/config");
        let original = std::fs::read(&config).unwrap();
        std::fs::create_dir(first.root.join("nested")).unwrap();
        std::fs::write(&config, "[broken\n").unwrap();
        assert!(matches!(pane.observe(first.root.join("nested")), External::Redraw));
        assert_eq!(pane.root, first.root);
        assert_eq!(pane.note, Some(DISCOVERY_FAILED));
        std::fs::write(&config, original).unwrap();
        pane.retry.as_mut().unwrap().1 = Instant::now();
        pane.check();
        assert_eq!(pane.note, None);
        assert_eq!(pane.last_dir, Some(first.root.join("nested")));
        assert!(matches!(pane.observe(second.root.clone()), External::NewScope));
        assert_eq!(pane.watch_scope().roots.first(), Some(&pane.root));
        assert!(matches!(
            pane.pane.reader_opener(),
            Some(OpenReader::Floating(Some(open))) if open.block == "block:previous"
        ));
        let mut screen = Screen::sized(30, 8);
        pane.draw(&mut screen);
        assert!(screen.text().iter().any(|row| row.contains("other.rs")));
        assert!(matches!(pane.observe(second.root.clone()), External::Unchanged));
    }

    #[test]
    fn first_same_worktree_cwd_after_promotion_keeps_the_selection_and_opener() {
        let f = Fixture::folder();
        f.write("sub/kept.txt", "keep\n");
        let make =
            |repo, prefs: &Preferences| -> Box<dyn Pane> { Box::new(Outline::with_preferences(repo, true, prefs)) };
        let repo = Repo::open(&f.root).unwrap();
        let mut pane = Project {
            root: repo.root.clone(),
            git: false,
            pane: make(repo, &Preferences::default()),
            make,
            source: None,
            last_dir: None,
            retry: None,
            note: None,
            notice_only: false,
            preference_note: None,
            focus: None,
            tab: None,
            host: None,
        };
        *pane.pane.reader_opener().unwrap() = OpenReader::Floating(Some(floated("block:kept")));
        let mut screen = Screen::sized(60, 8);
        pane.draw(&mut screen);
        pane.key(Key::Char('e'));
        pane.draw(&mut screen);
        pane.key(Key::Down);

        f.git(&["init", "-q", "--template=", "-b", "main"]);
        f.commit("first");
        assert!(pane.check());
        pane.draw(&mut screen);
        let before = screen.text();
        let selected = before.iter().position(|row| row.contains("kept.txt")).unwrap();
        assert!(screen.bar_at(selected), "{before:?}");

        assert!(matches!(pane.observe(f.root.join("sub")), External::Unchanged));
        pane.draw(&mut screen);
        assert_eq!(screen.text(), before);
        assert!(screen.bar_at(selected));
        assert!(matches!(
            pane.pane.reader_opener(),
            Some(OpenReader::Floating(Some(open))) if open.block == "block:kept"
        ));
    }

    #[test]
    fn a_notice_only_frame_suspends_inner_deadlines_and_clicks() {
        struct Due;
        impl Pane for Due {
            fn draw(&mut self, screen: &mut Screen) {
                screen.clear();
                screen.put(0, 0, screen.w, "Content", Style::fg(screen::YELLOW));
            }
            fn help(&self) -> Page {
                Page::new(vec![])
            }
            fn next_frame(&self) -> Option<Duration> {
                Some(Duration::ZERO)
            }
            fn key(&mut self, key: Key) -> After {
                if matches!(key, Key::Click(..) | Key::Enter) { After::Stay } else { After::Ignored }
            }
        }

        let mut pane = Project {
            root: PathBuf::from("/work/strata"),
            git: false,
            pane: Box::new(Due),
            make: |_: Repo, _: &Preferences| -> Box<dyn Pane> { Box::new(Due) },
            source: None,
            last_dir: None,
            retry: None,
            note: Some(NO_CWD),
            notice_only: false,
            preference_note: None,
            focus: None,
            tab: None,
            host: None,
        };
        let mut screen = Screen::sized(40, 1);
        pane.draw(&mut screen);
        assert_eq!(screen.text()[0].trim(), "Waiting for shell · showing strata");
        assert_eq!(pane.next_frame(), None);
        assert!(matches!(pane.key(Key::Click(1, 0)), After::Ignored));
        assert_eq!(pane.key(Key::Enter), After::Ignored);

        screen.resize(40, 2);
        pane.draw(&mut screen);
        assert_eq!(screen.text()[0], "Content");
        assert!(screen.text()[1].contains(NO_CWD));
        assert_eq!(pane.next_frame(), Some(Duration::ZERO));
        assert_eq!(pane.key(Key::Click(1, 0)), After::Stay);
        assert_eq!(pane.key(Key::Enter), After::Stay);

        screen.resize(40, 1);
        pane.draw(&mut screen);
        assert_eq!(pane.next_frame(), None);

        assert!(matches!(pane.set_note(None), External::Redraw));
        pane.draw(&mut screen);
        assert_eq!(screen.text()[0], "Content");
        assert_eq!(pane.next_frame(), Some(Duration::ZERO));
    }

    #[test]
    fn a_followed_history_shows_a_slow_commits_files_without_another_key() {
        use crate::history::History;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let f = Fixture::new();
        f.write("page.txt", "one\n");
        f.commit("first");
        let releases = Arc::new(AtomicUsize::new(0));
        let make = |repo, _: &Preferences| -> Box<dyn Pane> {
            Box::new(History::new(repo, true).impatient(Arc::clone(&releases)))
        };
        let repo = Repo::open(&f.root).unwrap();
        let mut pane = Project {
            root: repo.root.clone(),
            git: true,
            pane: make(repo, &Preferences::default()),
            make,
            source: None,
            last_dir: None,
            retry: None,
            note: None,
            notice_only: false,
            preference_note: None,
            focus: None,
            tab: None,
            host: None,
        };
        let mut screen = Screen::sized(60, 8);
        pane.draw(&mut screen);
        pane.key(Key::Enter);
        pane.draw(&mut screen);
        assert!(screen.text().iter().any(|row| row.contains("Reading files")), "{:?}", screen.text());
        releases.store(1, Ordering::Relaxed);

        // The loop is given only this pane's deadline and frames, never the one it wraps.
        let until = Instant::now() + Duration::from_secs(30);
        while !screen.text().iter().any(|row| row.contains("page.txt")) {
            let wait = pane.next_frame().expect("a frame is due while the files are read");
            assert!(Instant::now() < until, "{:?}", screen.text());
            std::thread::sleep(wait);
            pane.draw(&mut screen);
        }
        assert_eq!(pane.next_frame(), None);
    }

    #[test]
    fn a_shell_taken_to_another_host_lends_the_pane_there_and_gets_it_back() {
        let f = Fixture::new();
        f.write("first.txt", "one\n");
        f.commit("first");
        f.write("local.txt", "uncommitted here\n");
        let state = Fixture::folder();
        let make =
            |repo, prefs: &Preferences| -> Box<dyn Pane> { Box::new(Outline::with_preferences(repo, true, prefs)) };
        let repo = Repo::open(&f.root).unwrap();
        let project = f.root.file_name().unwrap().to_str().unwrap().to_owned();
        let mut pane = Project {
            root: repo.root.clone(),
            git: true,
            pane: make(repo, &Preferences::default()),
            make,
            source: None,
            last_dir: None,
            retry: None,
            note: None,
            notice_only: false,
            preference_note: None,
            focus: None,
            tab: Some(Tab {
                anchor: "block:1".into(),
                state: state.root.clone(),
                role: Role::Outline,
                foreground: Vec::new(),
                remote: Remote::Local,
            }),
            host: None,
        };
        let foot = |pane: &mut Project<_>| {
            let mut screen = Screen::sized(70, 6);
            pane.draw(&mut screen);
            screen.text()[5].trim().to_owned()
        };
        let me = Process::current().unwrap();
        let link = Link {
            owner: me,
            token: "a".repeat(32),
            host: "ub-1".into(),
            control: PathBuf::from("/tmp/milo-ssh-a/c"),
            config: None,
            helper: Some("/opt/bin/milo".into()),
        };
        link.write(&state.root, "block:1").unwrap();
        assert!(matches!(pane.look_remote(vec![1]), External::Unchanged), "another program is in front");
        assert!(pane.lend().is_none());

        assert!(matches!(pane.look_remote(vec![me.pid, 1]), External::Redraw));
        assert_eq!(foot(&mut pane), format!("ub-1 · Opening · showing {project}"));
        let lent = pane.lend().unwrap();
        assert!(lent.get_args().map(|arg| arg.to_str().unwrap()).collect::<Vec<_>>().ends_with(&[
            "exec",
            "/opt/bin/milo",
            "ssh-helper",
            "pane",
            "outline",
            &"a".repeat(32),
            "ub-1"
        ]));
        assert!(pane.lend().is_none(), "lent once");
        assert!(matches!(pane.look_remote(vec![me.pid]), External::Unchanged), "nothing moves while lent");
        assert_eq!(pane.returned(false), After::Stay);
        assert_eq!(foot(&mut pane), format!("ub-1 · Its pane ended · showing {project}"));
        assert!(pane.lend().is_none(), "an ended pane is not reopened on its own");

        link.remove(&state.root, "block:1");
        assert!(matches!(pane.look_remote(vec![1]), External::Redraw));
        assert_eq!(foot(&mut pane), "");
        let mut screen = Screen::sized(70, 6);
        while !pane.check() {}
        pane.draw(&mut screen);
        assert!(screen.text().iter().any(|row| row.contains("local.txt")), "{:?}", screen.text());

        let unfollowed = Link { token: "b".repeat(32), helper: None, ..link.clone() };
        unfollowed.write(&state.root, "block:1").unwrap();
        assert!(matches!(pane.look_remote(vec![me.pid]), External::Redraw));
        assert_eq!(foot(&mut pane), format!("ub-1 · Can't follow there · showing {project}"));
        assert!(pane.lend().is_none());

        let followed = Link { token: "c".repeat(32), ..link.clone() };
        followed.write(&state.root, "block:1").unwrap();
        pane.look_remote(vec![me.pid]);
        pane.lend().unwrap();
        assert_eq!(pane.returned(true), After::Leave, "q on the host closes this pane too");
        pane.look_remote(vec![1]);
        pane.look_remote(vec![me.pid]);
        pane.lend().unwrap();
        followed.remove(&state.root, "block:1");
        assert_eq!(pane.returned(false), After::Stay, "a logout or a lost connection brings it back");
        assert_eq!(foot(&mut pane), "");
    }

    #[test]
    fn a_pane_on_the_host_names_the_host_it_shows() {
        let f = Fixture::new();
        f.write("first.txt", "one\n");
        f.commit("first");
        f.write("remote.txt", "uncommitted there\n");
        let make =
            |repo, prefs: &Preferences| -> Box<dyn Pane> { Box::new(Outline::with_preferences(repo, false, prefs)) };
        let mut shell = std::process::Command::new("/bin/sleep").arg("30").current_dir(&f.root).spawn().unwrap();
        let process = Process::of(i32::try_from(shell.id()).unwrap()).unwrap();
        let repo = Repo::open(&f.root).unwrap();
        let mut pane = remote(repo, process, "dev@ub-1".into(), make);
        let mut screen = Screen::sized(70, 6);
        while !pane.check() {}
        pane.draw(&mut screen);
        let project = f.root.file_name().unwrap().to_str().unwrap();
        assert!(screen.text().iter().any(|row| row.contains("remote.txt")), "{:?}", screen.text());
        assert_eq!(screen.text()[5].trim(), format!("dev@ub-1 · showing {project}"));
        shell.kill().unwrap();
        shell.wait().unwrap();
    }
}
