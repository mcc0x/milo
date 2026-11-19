//! Retargets a maintained pane to the worktree of its explicitly chosen shell.

use std::ffi::OsStr;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::time::Duration;

use crate::git::Repo;
use crate::help::Page;
use crate::keys::Key;
use crate::pane::{After, External, Pane};
use crate::preferences::{Preferences, ProjectMode};
use crate::rex::{ShellChange, ShellObserver};
use crate::screen::{self, Screen, Style};
use crate::watch::Scope;

pub fn project(
    repo: Repo,
    shell: Option<&OsStr>,
    make: impl Fn(Repo, &Preferences) -> Box<dyn Pane> + 'static,
) -> Box<dyn Pane> {
    let (prefs, preference_note) = panel_preferences(&repo);
    let shell = shell.filter(|_| prefs.project_mode == ProjectMode::Follow);
    if shell.is_none() && preference_note.is_none() {
        return make(repo, &prefs);
    }
    let source = shell.and_then(ShellObserver::start);
    Box::new(Project {
        root: source.as_ref().map_or_else(PathBuf::new, |_| repo.root.clone()),
        git: repo.git,
        pane: make(repo, &prefs),
        make,
        note: (shell.is_some() && source.is_none()).then_some(NO_CWD),
        preference_note,
        source,
        last_dir: None,
        focus: None,
    })
}

fn panel_preferences(repo: &Repo) -> (Preferences, Option<&'static str>) {
    Preferences::for_pane(repo.git.then_some(repo.root.as_path()))
}

const NO_CWD: &str = "Pinned: no shell cwd";
const BAD_CWD: &str = "Pinned: cannot open cwd";

struct Project<F> {
    root: PathBuf,
    git: bool,
    pane: Box<dyn Pane>,
    make: F,
    source: Option<ShellObserver>,
    last_dir: Option<PathBuf>,
    note: Option<&'static str>,
    preference_note: Option<&'static str>,
    focus: Option<bool>,
}

impl<F: Fn(Repo, &Preferences) -> Box<dyn Pane>> Project<F> {
    fn observe(&mut self, dir: PathBuf) -> External {
        if self.last_dir.as_ref() == Some(&dir) {
            return self.set_note(None);
        }
        let Some(repo) = Repo::open(&dir) else {
            return self.set_note(Some(BAD_CWD));
        };
        self.last_dir = Some(dir);
        if self.root == repo.root && self.git == repo.git {
            return self.set_note(None);
        }
        let (prefs, preference_note) = panel_preferences(&repo);
        self.preference_note = preference_note;
        self.root.clone_from(&repo.root);
        self.git = repo.git;
        self.pane = (self.make)(repo, &prefs);
        if let Some(focused) = self.focus {
            self.pane.key(Key::Focus(focused));
        }
        if prefs.project_mode == ProjectMode::Pin {
            self.source = None;
        }
        self.note = None;
        External::NewScope
    }

    fn set_note(&mut self, note: Option<&'static str>) -> External {
        if std::mem::replace(&mut self.note, note) == note { External::Unchanged } else { External::Redraw }
    }
}

impl<F: Fn(Repo, &Preferences) -> Box<dyn Pane>> Pane for Project<F> {
    fn draw(&mut self, screen: &mut Screen) {
        let Some(note) = self.note.filter(|_| screen.h > 0) else {
            self.pane.draw(screen);
            return;
        };
        // The pane lays out only rows it can really draw into, so the notice
        // doesn't cover a selectable row or jump on top of a picture.
        screen.h -= 1;
        if screen.h > 0 {
            self.pane.draw(screen);
        } else {
            screen.clear();
        }
        screen.h += 1;
        screen.put_fit(1, screen.h - 1, screen.w.saturating_sub(2), note, Style::fg(screen::YELLOW));
    }

    fn notice(&self) -> Option<&'static str> {
        self.preference_note.or_else(|| self.pane.notice())
    }

    fn help(&self) -> Page {
        self.pane.help()
    }

    fn watch_scope(&self) -> Scope {
        self.pane.watch_scope()
    }

    fn git_busy(&self) -> bool {
        self.pane.git_busy()
    }

    fn check(&mut self) -> bool {
        self.pane.check()
    }

    fn filesystem_changed(&mut self) {
        self.pane.filesystem_changed();
    }

    fn external_fd(&self) -> Option<RawFd> {
        self.source.as_ref().map(ShellObserver::fd)
    }

    fn check_external(&mut self) -> External {
        match self.source.as_mut().map_or(ShellChange::Unchanged, ShellObserver::read) {
            ShellChange::Directory(dir) => self.observe(dir),
            ShellChange::Unreported => self.set_note(Some(NO_CWD)),
            ShellChange::Closed => {
                self.source = None;
                self.set_note(Some(NO_CWD))
            }
            ShellChange::Unchanged => External::Unchanged,
        }
    }

    fn next_frame(&self) -> Option<Duration> {
        self.pane.next_frame()
    }

    fn key(&mut self, key: Key) -> After {
        if let Key::Focus(focused) = key {
            self.focus = Some(focused);
        }
        self.pane.key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use crate::outline::Outline;

    #[test]
    fn retargets_both_pane_and_watcher_only_when_the_project_changes() {
        let first = Fixture::new();
        first.write("src/lib.rs", "first\n");
        first.commit("first");
        let second = Fixture::new();
        second.write("other.rs", "second\n");
        second.commit("second");
        let make =
            |repo, prefs: &Preferences| -> Box<dyn Pane> { Box::new(Outline::with_preferences(repo, false, prefs)) };
        let repo = Repo::open(&first.root).unwrap();
        let mut pane = Project {
            root: repo.root.clone(),
            git: true,
            pane: make(repo, &Preferences::default()),
            make,
            source: None,
            last_dir: None,
            note: None,
            preference_note: None,
            focus: Some(false),
        };
        assert!(matches!(pane.observe(first.root.join("src")), External::Unchanged));
        assert!(matches!(pane.observe(second.root.clone()), External::NewScope));
        assert_eq!(pane.watch_scope().roots.first(), Some(&pane.root));
        let mut screen = Screen::sized(30, 8);
        pane.draw(&mut screen);
        assert!(screen.text().iter().any(|row| row.contains("other.rs")));
        assert!(matches!(pane.observe(second.root.clone()), External::Unchanged));
    }
}
