//! The one background process that sets the Discord activity.
//!
//! It runs only while presence is on. It sleeps until Rex says an app opened
//! or left a session, or that what is in front changed; until Discord says
//! something; or until it is asked to stop. The only timer is the wait
//! before looking for a missing Discord again, and that runs only while a
//! Rex app has a session open.

use crate::sys::AsRawFd as _;
use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

use super::discord::{self, Connection, Ended, Failure};
use super::{Control, Details, Phase, label};
use crate::git;
use crate::rex::{AppActivity, AppObserver, Front, FrontParts};
use crate::setup::lock;
use crate::term;

/// How long to wait before looking for Discord again.
const RETRY: Duration = Duration::from_secs(10);
/// How long a helper that is being replaced is given to let go of the lock.
const TAKEOVER: Duration = Duration::from_secs(3);

/// Leaves the process that started the helper free to go on, then runs
/// until Rex stops or presence is turned off. `rex` is the program to watch Rex with.
pub(super) fn run(rex: &OsStr) -> i32 {
    let Some(control) = Control::real() else { return 1 };
    // While presence is off, the helper ends here, having only read the choice.
    if !control.setting().is_ok_and(|setting| setting.enabled) {
        return 0;
    }
    // SAFETY: Nothing has started a thread yet, so the child is a whole copy of this process.
    match unsafe { libc::fork() } {
        0 => {}
        -1 => return 1,
        _ => return 0,
    }
    // SAFETY: Starting a session touches no memory. It keeps a closing terminal from ending the helper.
    unsafe { libc::setsid() };
    // Do not keep the folder Milo was started in busy.
    let _ = std::env::set_current_dir("/");
    let Ok(stop) = term::termination() else { return 1 };
    // One helper at a time, so that two never set the activity in turn.
    let Ok(Some(_lock)) = lock::hold(&control.state, lock::PRESENCE_LOCK, TAKEOVER) else { return 0 };
    // A helper that was killed left its last note behind.
    control.forget_phase();
    // Read the choice again under the lock: turning presence off stops the lock's holder,
    // and a helper that takes the lock afterwards must find it off.
    let Ok(setting) = control.setting() else { return 1 };
    if !setting.enabled {
        return 0;
    }
    // The script asks Rex about sessions only for the names the person chose to show.
    let parts = FrontParts { session: setting.details.session(), directory: setting.details.project() };
    let Some(observer) = AppObserver::start(rex, parts) else { return 1 };
    let socket_dir = discord::socket_dir(|name| std::env::var_os(name));
    let mut helper = Helper {
        control: &control,
        application: setting.application_id(),
        details: setting.details,
        socket_dir: &socket_dir,
        connection: None,
        retry: Some(Instant::now()),
        names: None,
        shown: None,
        phase: None,
    };
    helper.watch(observer, stop.as_raw_fd());
    if let Some(connection) = helper.connection.take() {
        connection.clear();
    }
    control.forget_phase();
    0
}

struct Helper<'a> {
    control: &'a Control,
    application: u64,
    details: Details,
    socket_dir: &'a std::path::Path,
    connection: Option<Connection>,
    /// When to look for Discord next. `None` once Discord has refused, until the helper is replaced.
    retry: Option<Instant>,
    /// The second line that belongs with what is in front now.
    names: Option<String>,
    /// The second line Discord was last given, while the activity is up.
    shown: Option<Option<String>>,
    phase: Option<Phase>,
}

impl Helper<'_> {
    fn watch(&mut self, mut observer: AppObserver, stop: i32) {
        // Unknown until Rex first answers, so that nothing is noted or shown on a guess.
        let mut active = None;
        while !term::TERMINATED.load(std::sync::atomic::Ordering::Relaxed) {
            match active {
                Some(true) => self.show(),
                Some(false) => self.hide(),
                None => {}
            }
            let wait = match (&self.connection, self.retry) {
                (None, Some(at)) if active == Some(true) => {
                    term::milliseconds(at.saturating_duration_since(Instant::now()))
                }
                _ => -1,
            };
            let discord = self.connection.as_ref().map_or(-1, Connection::fd);
            let mut fds = [stop, observer.fd(), discord].map(|fd| crate::sys::pollfd {
                fd,
                events: crate::sys::POLLIN,
                revents: 0,
            });
            if term::wait_ready(&mut fds, wait) <= 0 {
                // A timeout, or a signal that the loop condition sees.
                continue;
            }
            if fds[0].revents != 0 {
                return;
            }
            if fds[1].revents != 0 {
                match observer.read() {
                    AppActivity::Active(front) => {
                        self.names = names(self.details, &front);
                        active = Some(true);
                    }
                    AppActivity::Idle => active = Some(false),
                    AppActivity::Unchanged => {}
                    AppActivity::Closed => return,
                }
            }
            if fds[2].revents != 0 {
                self.listen();
            }
        }
    }

    /// Puts the activity up if it is not up, and tells Discord again only
    /// when its second line is no longer the one that was sent.
    fn show(&mut self) {
        if self.connection.is_none() {
            if self.retry.is_none_or(|at| Instant::now() < at) {
                return;
            }
            match Connection::open(self.socket_dir, self.application) {
                Ok(connection) => {
                    self.connection = Some(connection);
                    self.shown = None;
                }
                Err(Failure::Missing) => return self.lose(),
                Err(Failure::UnknownApplication) => {
                    self.retry = None;
                    return self.note(Phase::Refused);
                }
            }
        }
        if self.shown.as_ref() == Some(&self.names) {
            return;
        }
        let sent =
            self.connection.as_mut().is_some_and(|connection| connection.set_activity(self.names.as_deref()).is_ok());
        if sent {
            self.shown = Some(self.names.clone());
            // A changed line is noted too, for `milo presence` to repeat.
            self.phase = None;
            self.note(Phase::Showing);
        } else {
            self.lose();
        }
    }

    /// Discord is not there, or stopped listening. Look for it again later.
    fn lose(&mut self) {
        self.connection = None;
        self.shown = None;
        self.retry = Some(Instant::now() + RETRY);
        self.note(Phase::Waiting);
    }

    /// Takes the activity down and lets go of Discord.
    fn hide(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.clear();
        }
        self.shown = None;
        self.names = None;
        if self.retry.is_some() {
            // Look for Discord at once when a Rex app next opens a session.
            self.retry = Some(Instant::now());
            self.note(Phase::Idle);
        }
    }

    fn listen(&mut self) {
        let Some(connection) = self.connection.as_mut() else { return };
        match connection.receive() {
            Ok(()) => {}
            Err(Ended::Lost) => self.lose(),
            Err(Ended::Rejected) => {
                if let Some(connection) = self.connection.take() {
                    connection.clear();
                }
                self.shown = None;
                self.retry = None;
                self.note(Phase::Rejected);
            }
        }
    }

    fn note(&mut self, phase: Phase) {
        if self.phase != Some(phase) {
            self.phase = Some(phase);
            let names = self.shown.as_ref().and_then(|names| names.as_deref()).filter(|_| phase == Phase::Showing);
            self.control.note_phase(phase, names);
        }
    }
}

/// The second line for what is in front, holding only the names the person
/// chose to show. A name that is unknown or unfit to show is left out.
fn names(details: Details, front: &Front) -> Option<String> {
    let session = front.session.as_deref().filter(|_| details.session()).and_then(label::display_name);
    let project = front
        .directory
        .as_deref()
        .filter(|_| details.project())
        .and_then(project_name)
        .and_then(|project| label::display_name(&project));
    label::line(session.as_deref(), project.as_deref())
}

/// The folder name of the Git repository `dir` is in, never where it is or
/// where it came from. A repository's own settings can name any folder as
/// its work tree, and a checkout that was unpacked or copied here has
/// settings someone else wrote. So a project is named only when Git's work
/// tree is the nearest folder that holds a `.git`, as a repository's, a
/// linked worktree's and a submodule's is. Any other would be named for a
/// folder above the checkout: part of where it is kept.
fn project_name(dir: &Path) -> Option<String> {
    let checkout = git::checkout(dir).ok().flatten()?;
    let dir = dir.canonicalize().ok()?;
    let holder = dir.ancestors().find(|folder| folder.join(".git").symlink_metadata().is_ok())?;
    git::same_folder(holder, &checkout.root).then_some(checkout.project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn only_the_chosen_names_are_shown_and_a_project_is_its_repository_folder_name() {
        let f = Fixture::new();
        f.write("src/deep/file.txt", "text\n");
        f.commit("first");
        let project = f.root.file_name().unwrap().to_str().unwrap().to_owned();
        let front = Front { session: Some("release notes".into()), directory: Some(f.root.join("src/deep")) };
        assert_eq!(names(Details::Generic, &front), None);
        assert_eq!(names(Details::Session, &front).as_deref(), Some("Session: release notes"));
        assert_eq!(names(Details::Project, &front), Some(format!("Project: {project}")));
        let both = names(Details::Both, &front).unwrap();
        assert_eq!(both, format!("Project: {project} · Session: release notes"));
        assert!(!both.contains('/'), "no part of the path is shown: {both}");

        // Outside a Git project there is no project to name, and the folder's own name is not used instead.
        let plain = Fixture::folder();
        let outside = Front { session: Some("/Users/someone/client".into()), directory: Some(plain.root.clone()) };
        assert_eq!(names(Details::Both, &outside), None);
        assert_eq!(names(Details::Both, &Front::default()), None);
    }

    #[test]
    fn a_work_tree_chosen_by_a_checkouts_own_settings_is_not_named() {
        let f = Fixture::new();
        f.write("file.txt", "text\n");
        f.commit("first");
        f.git(&["worktree", "add", "-q", "../feature"]);
        let project = |dir: std::path::PathBuf| names(Details::Project, &Front { session: None, directory: Some(dir) });
        assert_eq!(project(f.root.with_file_name("feature")).as_deref(), Some("Project: project"));

        // As in a folder unpacked from an archive, whose `.git/config` someone else wrote:
        // Git then takes the folder above the checkout for the project.
        f.git(&["config", "core.worktree", "../.."]);
        assert_eq!(project(f.root.clone()), None);
    }
}
