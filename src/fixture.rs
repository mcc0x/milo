//! Throwaway projects backed by real files and, when needed, real git.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Fixture {
    home: PathBuf,
    pub root: PathBuf,
}

impl Fixture {
    /// Creates an empty directory, without a repository.
    pub fn folder() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let home = loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = temp.join(format!("strata-test-{}-{n}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("cannot create {}: {e}", path.display()),
            }
        };
        let fixture = Self { root: home.join("project"), home };
        fs::create_dir(&fixture.root).unwrap();
        fixture
    }

    /// Creates an empty repository on `main`.
    pub fn new() -> Self {
        let fixture = Self::folder();
        fixture.git(&["init", "-q", "--template=", "-b", "main"]);
        fixture
    }

    /// Runs git in this project and returns its output.
    pub fn git(&self, args: &[&str]) -> String {
        let mut command = Command::new(&*crate::git::GIT);
        isolate_git(&mut command);
        let out = command.arg("-C").arg(&self.root).args(["-c", "commit.gpgsign=false"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    pub fn write(&self, path: &str, text: &str) {
        let full = self.root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, text).unwrap();
    }

    /// Commits all changes, including new and removed files.
    pub fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// Pushes `main` to a bare remote beside the project, creating it if needed.
    pub fn push(&self) {
        let bare = self.home.join("remote.git");
        if !bare.exists() {
            let bare = bare.to_str().unwrap();
            self.git(&["init", "-q", "--template=", "--bare", "-b", "main", bare]);
            self.git(&["remote", "add", "origin", bare]);
        }
        self.git(&["push", "-q", "-u", "origin", "main"]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.home) {
            assert!(std::thread::panicking(), "cannot remove {}: {e}", self.home.display());
        }
    }
}

/// Keeps both fixture commands and strata's own reads independent of user git settings.
pub(crate) fn isolate_git(command: &mut Command) {
    command
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Strata Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "2001-02-03T04:05:06Z")
        .env("GIT_COMMITTER_NAME", "Strata Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_DATE", "2001-02-03T04:05:06Z");
}
