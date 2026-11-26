//! Refreshes the history away from the pane loop, so that keys are answered
//! while Git reads the status of a large project.

use crate::sys::RawFd;
use std::io;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use crate::git::run::RefreshTask;
use crate::git::{Commit, HistoryVersion, Repo};

use super::{History, commits_since};

/// What a refresh brings back to the pane that asked for it.
struct Refreshed {
    /// The copy that was refreshed, to take the place of the one copied.
    repo: Repo,
    /// What to take in as a load would, unless nothing moved or Git could not answer.
    commits: Option<Option<Vec<Commit>>>,
}

/// One refresh on its own thread. Its descriptor becomes readable when the answer is ready.
pub(super) struct Running {
    ready: RefreshTask,
    answer: Receiver<Refreshed>,
}

impl Running {
    fn start(mut repo: Repo, version: Option<HistoryVersion>) -> io::Result<Self> {
        let (send, answer) = mpsc::sync_channel(1);
        let ready = RefreshTask::spawn(move || {
            let commits = if repo.poll(&[]) {
                repo.read();
                commits_since(&repo, version.as_ref())
            } else {
                None
            };
            let _ = send.send(Refreshed { repo, commits });
        })?;
        Ok(Self { ready, answer })
    }

    pub(super) fn fd(&self) -> RawFd {
        self.ready.fd()
    }
}

impl History {
    /// Starts a refresh from what is on screen. False if it could not be started.
    pub(super) fn start_refresh(&mut self) -> bool {
        self.refresh = Running::start(self.repo.clone(), self.version.clone()).ok();
        self.refresh.is_some()
    }

    /// Takes in a finished refresh as a check in the loop would have.
    /// `None` while it is still running or when there is none.
    pub(super) fn finish_refresh(&mut self) -> Option<bool> {
        let found = match self.refresh.as_ref()?.answer.try_recv() {
            Ok(found) => Some(found),
            Err(TryRecvError::Disconnected) => None,
            Err(TryRecvError::Empty) => return None,
        };
        self.refresh = None;
        // Git began changing the project while it was being read, so the answer
        // can be half of one state and half of another. The next look waits for Git.
        let Some(found) = found.filter(|_| !self.repo.busy()) else { return Some(false) };
        self.repo = found.repo;
        if let Some(commits) = found.commits {
            self.take_in(commits);
        }
        Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use crate::keys::Key;
    use crate::pane::Pane;
    use crate::screen::Screen;
    use crate::term;

    fn project() -> Fixture {
        let f = Fixture::new();
        f.write("page.txt", "first\n");
        f.commit("first commit");
        f
    }

    fn open(f: &Fixture) -> History {
        History::new(Repo::open(&f.root).unwrap(), false)
    }

    fn rows(history: &mut History) -> Vec<String> {
        let mut screen = Screen::sized(60, 14);
        history.draw(&mut screen);
        screen.text()
    }

    /// Waits for the running refresh to have its answer, then takes it in.
    fn finish(history: &mut History) -> Option<bool> {
        let fd = history.unfinished_check().expect("a refresh is running");
        let mut fds = [crate::sys::pollfd { fd, events: crate::sys::POLLIN, revents: 0 }];
        assert_eq!(term::wait_ready(&mut fds, 60_000), 1);
        history.finish_check()
    }

    /// Times cloning and dropping the refresh inputs, not starting the worker or running Git.
    #[test]
    #[ignore = "timing probe: run in release mode"]
    fn history_refresh_snapshot_profile() {
        use std::{hint::black_box, time::Instant};

        const COPIES: u32 = 512;
        for (files, dirty) in [(12, 2), (6_000, 1_000)] {
            let f = Fixture::new();
            f.write("00-probe.txt", "one\n");
            for i in 1..files {
                f.write(&format!("src/tracked-{i:05}.txt"), "tracked\n");
            }
            f.commit("probe base commit");
            f.git(&["remote", "add", "origin", "./never-contacted"]);
            f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
            f.git(&["branch", "--set-upstream-to=origin/main", "main"]);
            f.write("ahead.txt", "unpushed\n");
            f.commit("probe ahead commit");
            f.write("00-probe.txt", "one\ntwo\n");
            for i in 1..dirty {
                f.write(&format!("src/tracked-{i:05}.txt"), "modified\n");
            }
            let history = open(&f);
            let snapshot = || (history.repo.clone(), history.version.clone());
            let (repo, version) = snapshot();
            assert_eq!(repo.uncommitted_count(), dirty);
            assert!(version.unwrap().matches(&repo));
            assert_eq!(repo.history(1).unwrap()[0].subject, "probe ahead commit");

            let mut samples = [0; 21];
            for sample in &mut samples {
                let start = Instant::now();
                for _ in 0..COPIES {
                    drop(black_box(snapshot()));
                }
                *sample = start.elapsed().as_nanos() / u128::from(COPIES);
            }
            samples.sort_unstable();
            eprintln!("history refresh snapshot: tracked={} dirty={dirty} median={} ns", files + 1, samples[10]);
        }
    }

    #[test]
    fn a_refresh_away_from_the_loop_shows_what_a_check_in_the_loop_shows() {
        let f = project();
        let (mut away, mut inside) = (open(&f).refreshing_away(), open(&f));
        let before = rows(&mut away);
        f.write("page.txt", "second\n");
        f.commit("second commit");
        f.write("notes.txt", "not yet committed\n");

        assert!(away.check());
        // Nothing of a refresh is shown until all of it can be.
        assert_eq!(rows(&mut away), before);
        assert_eq!(finish(&mut away), Some(true));
        assert!(away.unfinished_check().is_none());

        inside.check();
        let shown = rows(&mut away);
        assert_eq!(shown, rows(&mut inside));
        assert!(shown.iter().any(|row| row.contains("second commit")), "{shown:?}");
        assert!(shown.iter().any(|row| row.contains("1 file to commit")), "{shown:?}");
    }

    #[test]
    fn a_refresh_that_finds_only_uncommitted_work_keeps_the_commits_and_the_cursor() {
        let f = project();
        f.write("page.txt", "second\n");
        f.commit("second commit");
        let mut history = open(&f).refreshing_away();
        rows(&mut history);
        history.key(Key::Down);
        let selected = history.selected();
        f.write("notes.txt", "not yet committed\n");

        assert!(history.check());
        assert_eq!(finish(&mut history), Some(true));
        assert_eq!(history.uncommitted, 1);
        assert!(history.selected() == selected);
    }

    #[test]
    fn one_refresh_runs_at_a_time_and_a_reload_drops_the_one_running() {
        let f = project();
        let mut history = open(&f).refreshing_away();
        assert!(history.check());
        let first = history.unfinished_check();
        assert!(history.check());
        assert_eq!(history.unfinished_check(), first);

        f.write("page.txt", "second\n");
        f.commit("second commit");
        history.key(Key::Char('r'));
        assert!(history.unfinished_check().is_none());
        assert_eq!(history.finish_check(), None);
        assert!(rows(&mut history).iter().any(|row| row.contains("second commit")));
    }

    #[test]
    fn an_answer_read_while_git_changes_the_project_is_not_shown() {
        let f = project();
        let mut history = open(&f).refreshing_away();
        let before = rows(&mut history);
        f.write("page.txt", "second\n");
        f.commit("second commit");
        assert!(history.check());
        let fd = history.unfinished_check().unwrap();
        let mut fds = [crate::sys::pollfd { fd, events: crate::sys::POLLIN, revents: 0 }];
        assert_eq!(term::wait_ready(&mut fds, 60_000), 1);
        f.write(".git/index.lock", "");

        assert_eq!(history.finish_check(), Some(false));
        assert_eq!(rows(&mut history), before);
    }
}
