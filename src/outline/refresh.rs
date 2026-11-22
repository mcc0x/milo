//! Refreshes the outline away from the pane loop, so that keys are answered
//! while Git reads a large project.

use std::io::{self, PipeReader};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::git::{Kind, PendingFile, Refreshed, Repo};

use super::Outline;

/// One refresh on its own thread. Its descriptor becomes readable when the answer is ready.
pub(super) struct Running {
    ready: PipeReader,
    answer: Receiver<Refreshed>,
    /// What the reader sharing the pane said of itself when this started.
    reader_settled: bool,
}

impl Running {
    fn start(repo: Repo, files: Vec<PendingFile>, reader_settled: bool) -> io::Result<Self> {
        let (ready, done) = io::pipe()?;
        let (send, answer) = mpsc::sync_channel(1);
        thread::Builder::new().spawn(move || {
            let _ = send.send(repo.refreshed(&files));
            // Closing the write end wakes the pane loop after the answer is ready.
            drop(done);
        })?;
        Ok(Self { ready, answer, reader_settled })
    }

    pub(super) fn fd(&self) -> RawFd {
        self.ready.as_raw_fd()
    }
}

impl Outline {
    /// Starts a refresh from what is on screen. False if it could not be started.
    pub(super) fn start_refresh(&mut self, reader_settled: bool) -> bool {
        // A poll compares the stamps of tracked files only.
        let files = self.pending.iter().filter(|file| file.kind != Kind::Untracked).cloned().collect();
        self.refresh = Running::start(self.repo.clone(), files, reader_settled).ok();
        self.refresh.is_some()
    }

    /// Takes in a finished refresh as a check in the loop would have.
    /// `None` while it is still running or when there is none.
    pub(super) fn finish_refresh(&mut self) -> Option<bool> {
        let running = self.refresh.as_ref()?;
        let found = match running.answer.try_recv() {
            Ok(found) => Some(found),
            Err(TryRecvError::Disconnected) => None,
            Err(TryRecvError::Empty) => return None,
        };
        let settled = running.reader_settled;
        self.refresh = None;
        // Git began changing the project while it was being read, so the answer
        // can be half of one state and half of another. The next look waits for Git.
        let Some(found) = found.filter(|_| !self.repo.busy()) else { return Some(false) };
        self.repo = found.repo;
        if let Some(files) = found.pending {
            let old = self.take_in(files);
            self.mark_changes(&old);
        }
        Some(settled)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::fixture::Fixture;
    use crate::keys::Key;
    use crate::pane::Pane;
    use crate::preferences::Preferences;
    use crate::screen::Screen;
    use crate::term;

    fn project() -> Fixture {
        let f = Fixture::new();
        f.write("src/main.rs", "fn main() {}\n");
        f.write("src/old.rs", "fn old() {}\n");
        f.write("gone.txt", "soon gone\n");
        f.commit("first");
        f
    }

    /// Marks that do not pulse, so that two outlines drawn at different moments can be compared.
    fn open(f: &Fixture) -> Outline {
        let prefs = Preferences { static_marks: true, ..Preferences::default() };
        Outline::with_preferences(Repo::open(&f.root).unwrap(), false, &prefs)
    }

    fn rows(o: &mut Outline) -> Vec<String> {
        let mut screen = Screen::sized(60, 14);
        o.draw(&mut screen);
        screen.text()
    }

    fn shows(o: &mut Outline, name: &str) -> bool {
        rows(o).iter().any(|row| row.contains(name))
    }

    /// Waits for the running refresh to have its answer, then takes it in.
    fn finish(o: &mut Outline) -> Option<bool> {
        let fd = o.unfinished_check().expect("a refresh is running");
        let mut fds = [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }];
        assert_eq!(term::wait_ready(&mut fds, 60_000), 1);
        o.finish_check()
    }

    /// Times the part of an automatic Git refresh during which Outline cannot
    /// answer a key, separately from the time the background worker takes.
    #[test]
    #[ignore = "a benchmark; run alone with --release --ignored --nocapture"]
    fn adopting_edits_in_a_large_worktree() {
        use std::time::{Duration, Instant};

        let f = Fixture::new();
        for folder in 0..100 {
            let dir = f.root.join(format!("pkg{folder:03}"));
            fs::create_dir(&dir).unwrap();
            for file in 0..500 {
                fs::write(dir.join(format!("file{file:03}")), "base\n").unwrap();
            }
        }
        f.commit("base");
        let mut o = open(&f).refreshing_away();
        let mut samples = [Vec::new(), Vec::new(), Vec::new()];
        for lines in 1..=21 {
            f.write("pkg000/file000", &"edit\n".repeat(lines));
            let start = Instant::now();
            o.check();
            let away = Instant::now();
            let fd = o.unfinished_check().unwrap();
            let mut fds = [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }];
            assert_eq!(term::wait_ready(&mut fds, 60_000), 1);
            let ready = Instant::now();
            assert_eq!(o.finish_check(), Some(true));
            let done = Instant::now();
            let row = o.tree.row_of("pkg000/file000").and_then(|at| o.tree.row(at)).unwrap();
            assert_eq!((row.add, row.del), (lines as u64, 1));
            // The first edit makes a previously clean file pending.
            if lines > 1 {
                for (samples, elapsed) in samples.iter_mut().zip([away - start, ready - away, done - ready]) {
                    samples.push(elapsed);
                }
            }
        }
        for (label, mut samples) in ["launch", "worker", "adopt"].into_iter().zip(samples) {
            samples.sort_unstable();
            let us = |d: Duration| d.as_micros();
            println!("{label}\t20 samples\tp50={} us\tp95={} us", us(samples[9]), us(samples[18]));
        }
    }

    #[test]
    fn a_refresh_away_from_the_loop_shows_what_a_check_in_the_loop_shows() {
        let f = project();
        let (mut away, mut inside) = (open(&f).refreshing_away(), open(&f));
        let before = rows(&mut away);
        f.write("src/main.rs", "fn main() {\n    run();\n}\n");
        f.write("notes.txt", "one\ntwo\n");
        f.git(&["mv", "src/old.rs", "src/new.rs"]);
        fs::remove_file(f.root.join("gone.txt")).unwrap();

        assert!(away.check());
        // Nothing of a refresh is shown until all of it can be.
        assert_eq!(rows(&mut away), before);
        assert_eq!(finish(&mut away), Some(true));
        assert!(away.unfinished_check().is_none());

        inside.check();
        let shown = rows(&mut away);
        assert_eq!(shown, rows(&mut inside));
        assert!(shown[0].ends_with("4 to commit"), "{shown:?}");
        for name in ["main.rs", "notes.txt", "new.rs", "gone.txt"] {
            assert!(shown.iter().any(|row| row.contains(name)), "{name} in {shown:?}");
        }
    }

    #[test]
    fn a_check_does_not_start_a_second_refresh_beside_one_that_is_running() {
        let f = project();
        let mut o = open(&f).refreshing_away();
        f.write("notes.txt", "one\n");
        o.check();
        let first = o.unfinished_check();
        o.check();
        assert_eq!(o.unfinished_check(), first);
        assert_eq!(finish(&mut o), Some(true));
        assert!(shows(&mut o, "notes.txt"));
    }

    #[test]
    fn a_manual_reload_supersedes_a_refresh_that_is_still_running() {
        let f = project();
        let mut o = open(&f).refreshing_away();
        f.write("notes.txt", "one\n");
        o.check();
        f.write("later.txt", "two\n");
        o.key(Key::Char('r'));
        // The reload read Git after the refresh began, so the refresh has nothing newer.
        assert!(o.unfinished_check().is_none());
        assert_eq!(o.finish_check(), None);
        assert!(shows(&mut o, "notes.txt") && shows(&mut o, "later.txt"));
    }

    #[test]
    fn a_refresh_that_ends_while_git_holds_the_index_is_not_taken_in() {
        let f = project();
        let mut o = open(&f).refreshing_away();
        let before = rows(&mut o);
        f.write("notes.txt", "one\n");
        o.check();
        let lock = f.root.join(".git/index.lock");
        fs::write(&lock, "").unwrap();
        assert_eq!(finish(&mut o), Some(false));
        assert_eq!(rows(&mut o), before);

        fs::remove_file(lock).unwrap();
        o.check();
        assert_eq!(finish(&mut o), Some(true));
        assert!(shows(&mut o, "notes.txt"));
    }

    #[test]
    fn line_counts_made_while_a_refresh_runs_are_kept_when_it_is_taken_in() {
        let f = project();
        f.write("notes.txt", "one\ntwo\n");
        let mut o = open(&f).refreshing_away();
        let counted = |o: &Outline| {
            let notes = o.pending.iter().find(|file| file.path == "notes.txt").unwrap();
            (notes.add, notes.lines_pending)
        };
        assert_eq!(counted(&o), (0, true));
        f.write("later.txt", "three\n");
        o.check();
        rows(&mut o);
        assert_eq!(counted(&o), (2, false));
        assert_eq!(finish(&mut o), Some(true));
        assert_eq!(counted(&o), (2, false));
        assert!(o.pending.iter().any(|file| file.path == "later.txt"));
    }

    #[test]
    fn a_refresh_git_cannot_answer_keeps_the_tree_and_says_so() {
        let f = project();
        let mut o = open(&f).refreshing_away();
        let before = rows(&mut o);
        let index = f.root.join(".git/index");
        let saved = fs::read(&index).unwrap();
        fs::write(&index, "corrupt index").unwrap();
        o.check();
        assert_eq!(finish(&mut o), Some(true));
        let failed = rows(&mut o);
        assert!(failed[0].ends_with("git did not answer"), "{failed:?}");
        assert_eq!(failed[1..], before[1..]);

        fs::write(index, saved).unwrap();
        o.check();
        assert_eq!(finish(&mut o), Some(true));
        assert_eq!(rows(&mut o), before);
    }
}
