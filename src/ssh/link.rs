//! The note `milo ssh` leaves for the panes of its tab while its shell is on
//! another computer.
//!
//! A *link* says which host the shell reached, how to reach it again over the
//! same connection, and whether Milo is there to show it. It is kept in a
//! private file named after the Rex block of the shell, and it counts only
//! while the `milo ssh` that wrote it is running in the foreground of that
//! same shell, as Rex reports it. Nothing a terminal printed can make or move
//! a link.

use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

use super::process::Process;
use crate::fs_ops;

const FORMAT: &str = "milo-ssh 1";
const SIZE_LIMIT: u64 = 16 << 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The `milo ssh` that holds the connection.
    pub owner: Process,
    /// Names this connection on both computers.
    pub token: String,
    /// The host as the person typed it, which is also how it is shown.
    pub host: String,
    pub control: PathBuf,
    /// The ssh configuration file the person named with -F.
    pub config: Option<PathBuf>,
    /// Milo on the host, or none when it could not be found there.
    pub helper: Option<String>,
}

impl Link {
    /// The link of the shell in `block`, if its `milo ssh` or a program it
    /// started is in front of the shell now.
    pub fn find(state: &Path, block: &str, foreground: &[i32]) -> Option<Self> {
        let link = Self::read(&tabs(state).join(block))?;
        foreground.iter().any(|&pid| link.owner.leads(pid)).then_some(link)
    }

    /// The link of the shell in `block` while its `milo ssh` runs, wherever
    /// that is in the shell. For a pane that only offers a choice.
    pub fn held(state: &Path, block: &str) -> Option<Self> {
        Self::read(&tabs(state).join(block)).filter(|link| link.owner.alive())
    }

    /// Whether the link is still the one in place for `block`.
    pub fn current(&self, state: &Path, block: &str) -> bool {
        Self::read(&tabs(state).join(block)).is_some_and(|link| link == *self && link.owner.alive())
    }

    pub fn write(&self, state: &Path, block: &str) -> io::Result<()> {
        let dir = tabs(state);
        #[cfg(unix)]
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        #[cfg(windows)]
        fs::create_dir_all(&dir)?;
        fs_ops::open_dir(&dir)?;
        let (temp, mut file) = fs_ops::Temp::create(dir.join(format!(".{block}.{}", self.token)))?;
        file.write_all(self.encode().as_bytes())?;
        file.sync_all()?;
        temp.replace(&dir.join(block))
    }

    /// Removes the link of `block` if it is still this one.
    pub fn remove(&self, state: &Path, block: &str) {
        let path = tabs(state).join(block);
        if Self::read(&path).is_some_and(|link| link.token == self.token) {
            let _ = fs::remove_file(path);
        }
    }

    fn encode(&self) -> String {
        let path =
            |path: Option<&Path>| path.map_or_else(|| "-".to_owned(), |path| hex(path.as_os_str().as_encoded_bytes()));
        format!(
            "{FORMAT}\nowner {} {}\ntoken {}\nhost {}\ncontrol {}\nconfig {}\nhelper {}\n",
            self.owner.pid,
            self.owner.start,
            self.token,
            self.host,
            path(Some(&self.control)),
            path(self.config.as_deref()),
            self.helper.as_deref().unwrap_or("-"),
        )
    }

    fn read(path: &Path) -> Option<Self> {
        let mut file = fs_ops::open_read(path).ok()?;
        let meta = file.metadata().ok()?;
        #[cfg(unix)]
        // SAFETY: geteuid has no preconditions.
        let mine = meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0;
        #[cfg(windows)]
        let mine = false;
        if !mine || !meta.is_file() || meta.len() > SIZE_LIMIT {
            return None;
        }
        let mut text = String::new();
        (&mut file as &mut File).take(SIZE_LIMIT).read_to_string(&mut text).ok()?;
        Self::decode(&text)
    }

    fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        (lines.next()? == FORMAT).then_some(())?;
        let mut field = |name: &str| lines.next()?.strip_prefix(name)?.strip_prefix(' ');
        let mut owner = field("owner")?.split(' ');
        let owner = Process { pid: owner.next()?.parse().ok()?, start: owner.next()?.parse().ok()? };
        let token = field("token").filter(|token| is_token(token))?.to_owned();
        let host = field("host").filter(|host| is_host(host))?.to_owned();
        let control = path(field("control")?)?;
        let config = match field("config")? {
            "-" => None,
            code => Some(path(code)?),
        };
        let helper = match field("helper")? {
            "-" => None,
            helper if is_helper(helper) => Some(helper.to_owned()),
            _ => return None,
        };
        Some(Self { owner, token, host, control, config, helper })
    }
}

fn tabs(state: &Path) -> PathBuf {
    state.join("ssh").join("tabs")
}

fn path(code: &str) -> Option<PathBuf> {
    let bytes = unhex(code)?;
    crate::sys::os_string(bytes).map(PathBuf::from).filter(|path| path.is_absolute())
}

/// A connection's name: 32 lowercase hex digits.
pub fn is_token(word: &str) -> bool {
    word.len() == 32 && word.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A host as ssh takes it, in characters that mean nothing to a shell or a terminal.
pub fn is_host(word: &str) -> bool {
    (1..=255).contains(&word.len())
        && !word.starts_with('-')
        && word.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-@:+%".contains(&b))
}

/// Milo's path on the host, in characters that need no quoting in any shell.
pub fn is_helper(word: &str) -> bool {
    (2..=1024).contains(&word.len())
        && word.starts_with('/')
        && !word.split('/').any(|part| part == "..")
        && word.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._+-".contains(&b))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(code: &str) -> Option<Vec<u8>> {
    if code.is_empty() || !code.len().is_multiple_of(2) {
        return None;
    }
    let digit = |b: u8| (b as char).to_digit(16).filter(|_| !b.is_ascii_uppercase());
    let bytes: Option<Vec<u8>> = code
        .as_bytes()
        .chunks(2)
        .map(|pair| Some(u8::try_from(digit(pair[0])? * 16 + digit(pair[1])?).ok()?).filter(|&b| b != 0))
        .collect();
    bytes
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::os::unix::fs::PermissionsExt as _;

    fn link(owner: Process) -> Link {
        Link {
            owner,
            token: "0123456789abcdef0123456789abcdef".into(),
            host: "dev@ub-1".into(),
            control: PathBuf::from("/tmp/milo-ssh-0123/c"),
            config: Some(PathBuf::from("/a dir/with\nnewline")),
            helper: Some("/home/dev/.local/bin/milo".into()),
        }
    }

    #[test]
    fn a_link_counts_only_while_its_owner_runs_in_the_shells_foreground() {
        let state = Fixture::folder();
        let me = Process::current().unwrap();
        let ours = link(me);
        ours.write(&state.root, "block:1").unwrap();
        let dir = state.root.join("ssh/tabs");
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(dir.join("block:1")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "no temporary file is left");

        assert_eq!(Link::find(&state.root, "block:1", &[1, me.pid]), Some(ours.clone()));
        assert_eq!(Link::find(&state.root, "block:1", &[1]), None, "another program is in front");
        assert_eq!(Link::find(&state.root, "block:2", &[me.pid]), None, "another tab's shell");
        assert!(ours.current(&state.root, "block:1"));

        let gone = link(Process { start: me.start + 1, ..me });
        gone.write(&state.root, "block:3").unwrap();
        assert_eq!(Link::find(&state.root, "block:3", &[me.pid]), None, "a reused process ID");

        let other = Link { token: "f".repeat(32), ..ours.clone() };
        other.remove(&state.root, "block:1");
        assert!(dir.join("block:1").exists(), "only the owner removes its link");
        ours.remove(&state.root, "block:1");
        assert!(!dir.join("block:1").exists());
        assert!(!ours.current(&state.root, "block:1"));
    }

    #[test]
    fn a_link_others_could_write_or_that_names_an_unsafe_word_is_ignored() {
        let state = Fixture::folder();
        let me = Process::current().unwrap();
        link(me).write(&state.root, "block:1").unwrap();
        let file = state.root.join("ssh/tabs/block:1");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o620)).unwrap();
        assert_eq!(Link::find(&state.root, "block:1", &[me.pid]), None);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Link::find(&state.root, "block:1", &[me.pid]).is_some());

        let text = fs::read_to_string(&file).unwrap();
        for (from, to) in [
            ("host dev@ub-1", "host dev@ub-1;rm"),
            ("host dev@ub-1", "host -oProxyCommand=x"),
            ("helper /home/dev/.local/bin/milo", "helper /home/dev/my milo"),
            ("helper /home/dev/.local/bin/milo", "helper milo"),
            ("helper /home/dev/.local/bin/milo", "helper /home/../milo"),
            ("token 0123456789abcdef0123456789abcdef", "token 0123"),
        ] {
            assert!(Link::decode(&text.replace(from, to)).is_none(), "{to}");
        }
        assert_eq!(Link::decode(&text.replace("helper /home/dev/.local/bin/milo", "helper -")).unwrap().helper, None);
    }

    #[test]
    fn hosts_and_helpers_are_words_a_shell_reads_as_they_are() {
        for host in ["ub-1", "dev@ub-1.local", "10.0.0.2", "fe80::1%en0", "host_name"] {
            assert!(is_host(host), "{host}");
        }
        for host in ["", "-F", "a b", "a;b", "a'b", "a$b", "a\x1bb", "a\nb", "a*b", "[::1]"] {
            assert!(!is_host(host), "{host:?}");
        }
        assert!(is_helper("/usr/local/bin/milo"));
        assert!(!is_helper("/opt/my tools/milo"));
    }
}
