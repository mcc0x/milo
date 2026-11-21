//! Says which computer this Milo runs on, which Rex serves it, and whether
//! that Rex is one Milo was checked with.
//!
//! Someone with several Rex hosts sees the same Milo on each. The name here
//! is what tells them apart in `milo doctor` and in the browser editor,
//! above all when the editor of another host is open on this computer.

use std::fmt::Write as _;

use crate::rex::{self, CommandVersion, Server, Viewers};

/// The revision of Rex's control API that Milo's Lua files and calls were checked against.
pub(crate) const CHECKED_API: u64 = 18;

const OLDER: &str = "This Rex server is older than the Rex Milo was checked with. Update Rex on this host.";
const COMMAND_DIFFERS: &str =
    "The rex command and the Rex server are different versions. Restart or update Rex before saving a layout.";

pub(crate) struct Host {
    /// This computer's own short name. It stays on this computer and in pages it serves.
    pub name: String,
    pub rex: Option<Rex>,
    /// Whether this editor was started to be opened from another computer.
    pub forwarded: bool,
}

/// A Rex server that answered, and what could be learned beside it.
pub(crate) struct Rex {
    pub server: Server,
    pub command: Option<CommandVersion>,
    pub viewers: Option<Viewers>,
}

/// How the Rex on this host compares with the one Milo was checked with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Compatibility {
    Checked,
    /// Rex keeps older calls working, so this is said but is no warning.
    Newer,
    Older,
    /// `rex config check` runs in the command, so a file it accepts can still be misread by the server.
    CommandDiffers,
}

impl Compatibility {
    fn name(self) -> &'static str {
        match self {
            Self::Checked => "checked",
            Self::Newer => "newer",
            Self::Older => "older",
            Self::CommandDiffers => "command_differs",
        }
    }

    /// What Health and Doctor warn about, if anything.
    pub(crate) fn warning(self) -> Option<&'static str> {
        match self {
            Self::Checked | Self::Newer => None,
            Self::Older => Some(OLDER),
            Self::CommandDiffers => Some(COMMAND_DIFFERS),
        }
    }

    fn note(self) -> &'static str {
        match self {
            Self::Checked => "This is the Rex API Milo was checked with.",
            Self::Newer => "Newer than the Rex API Milo was checked with. Rex keeps older calls working.",
            Self::Older => OLDER,
            Self::CommandDiffers => COMMAND_DIFFERS,
        }
    }
}

impl Rex {
    pub(crate) fn compatibility(&self) -> Compatibility {
        if self.server.api < CHECKED_API {
            Compatibility::Older
        } else if self.command.as_ref().is_some_and(|command| command.api != self.server.api) {
            Compatibility::CommandDiffers
        } else if self.server.api > CHECKED_API {
            Compatibility::Newer
        } else {
            Compatibility::Checked
        }
    }

    /// Enough of Rex's host ID to tell two hosts apart at a glance.
    fn short_id(&self) -> &str {
        &self.server.host_id[..8]
    }
}

impl Host {
    /// Asks this computer and the Rex server of this terminal. It starts no server.
    pub(crate) fn probe(forwarded: bool) -> Option<Self> {
        if cfg!(test) {
            return None;
        }
        let rex =
            rex::server(None).map(|server| Rex { server, command: rex::command_version(), viewers: rex::viewers() });
        Some(Self { name: computer_name(), rex, forwarded })
    }

    pub(crate) fn warning(&self) -> Option<&'static str> {
        self.rex.as_ref().and_then(|rex| rex.compatibility().warning())
    }

    /// Whether every Rex app showing this host runs somewhere else, so a
    /// browser opened here would be on a screen nobody is looking at.
    pub(crate) fn seen_from_elsewhere(&self) -> bool {
        self.rex.as_ref().is_some_and(|rex| rex.viewers == Some(Viewers::Elsewhere))
    }

    /// The lines `milo doctor` prints about the host.
    pub(crate) fn report(&self, out: &mut String) {
        let Some(rex) = &self.rex else {
            writeln!(out, "Host: {}", self.name).unwrap();
            out.push_str("Rex server: none answered; the panes work without it\n");
            return;
        };
        writeln!(out, "Host: {} (Rex host {})", self.name, rex.short_id()).unwrap();
        write!(out, "Rex server: {}, API {}", rex.server.version, rex.server.api).unwrap();
        match &rex.command {
            Some(command) if command.version == rex.server.version && command.api == rex.server.api => {
                out.push_str("; rex command: the same")
            }
            Some(command) => write!(out, "; rex command: {}, API {}", command.version, command.api).unwrap(),
            None => out.push_str("; rex command: not found on PATH"),
        }
        out.push('\n');
        let compatibility = rex.compatibility();
        writeln!(out, "Rex compatibility: checked with API {CHECKED_API}. {}", compatibility.note()).unwrap();
        out.push_str(match rex.viewers {
            Some(Viewers::Here) => "Rex apps: open on this computer\n",
            Some(Viewers::Elsewhere) => {
                "Rex apps: connected from another device only; on that computer, milo web --host LABEL opens these settings\n"
            }
            Some(Viewers::Nobody) => "Rex apps: none connected\n",
            None => "Rex apps: unknown\n",
        });
    }

    /// What the browser editor shows about the host.
    pub(crate) fn json(&self, out: &mut String) {
        write!(out, "{{\"name\":{},\"forwarded\":{},\"rex\":", rex::json(&self.name), self.forwarded).unwrap();
        let Some(rex) = &self.rex else {
            out.push_str("null}");
            return;
        };
        let compatibility = rex.compatibility();
        let viewers = match rex.viewers {
            Some(Viewers::Here) => "\"here\"",
            Some(Viewers::Elsewhere) => "\"elsewhere\"",
            Some(Viewers::Nobody) => "\"nobody\"",
            None => "null",
        };
        write!(
            out,
            "{{\"host_id\":\"{}\",\"version\":\"{}\",\"api\":{},\"checked_api\":{CHECKED_API},\"command\":",
            rex.short_id(),
            rex.server.version,
            rex.server.api,
        )
        .unwrap();
        match &rex.command {
            Some(command) => write!(out, "{{\"version\":\"{}\",\"api\":{}}}", command.version, command.api).unwrap(),
            None => out.push_str("null"),
        }
        write!(
            out,
            ",\"compatibility\":\"{}\",\"warning\":{},\"note\":{},\"viewers\":{viewers}}}}}",
            compatibility.name(),
            compatibility.warning().is_some(),
            rex::json(compatibility.note()),
        )
        .unwrap();
    }
}

/// The first label of this computer's host name, kept to letters a page or a
/// terminal shows as they are.
fn computer_name() -> String {
    // A development build can be shown with a made-up name, for a test or a screenshot.
    #[cfg(debug_assertions)]
    if let Some(name) = std::env::var_os("MILO_TEST_HOST_NAME").and_then(|name| name.into_string().ok()) {
        return short_name(name.as_bytes());
    }
    let mut buffer = [0u8; 256];
    // SAFETY: The buffer is writable for the length given, and one byte is
    // kept back so that the name always ends in a NUL.
    let failed = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len() - 1) } != 0;
    let len = buffer.iter().position(|&byte| byte == 0).unwrap_or(0);
    short_name(if failed { &[] } else { &buffer[..len] })
}

fn short_name(full: &[u8]) -> String {
    let label = full.split(|&byte| byte == b'.').next().unwrap_or_default();
    let name: String = label
        .iter()
        .take(63)
        .map(|&byte| char::from(byte))
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    if name.is_empty() { "this computer".into() } else { name }
}

#[cfg(test)]
mod tests {
    use super::{CHECKED_API, Compatibility, Host, Rex, short_name};
    use crate::rex::{CommandVersion, Server, Viewers};

    fn host(server_api: u64, command_api: Option<u64>, viewers: Viewers) -> Host {
        let server =
            Server { version: "0.1.0".into(), api: server_api, host_id: "d4744373-81e8-4cdb-a5be-9a5be80c2c4a".into() };
        let command = command_api.map(|api| CommandVersion { version: "0.1.0".into(), api });
        Host { name: "studio".into(), rex: Some(Rex { server, command, viewers: Some(viewers) }), forwarded: false }
    }

    fn compatibility(server_api: u64, command_api: Option<u64>) -> Compatibility {
        host(server_api, command_api, Viewers::Here).rex.unwrap().compatibility()
    }

    #[test]
    fn only_an_older_server_or_a_mismatched_command_is_a_warning() {
        assert_eq!(compatibility(CHECKED_API, Some(CHECKED_API)), Compatibility::Checked);
        assert_eq!(compatibility(CHECKED_API, None), Compatibility::Checked);
        assert_eq!(compatibility(CHECKED_API + 1, Some(CHECKED_API + 1)), Compatibility::Newer);
        assert_eq!(compatibility(CHECKED_API - 1, Some(CHECKED_API - 1)), Compatibility::Older);
        assert_eq!(compatibility(CHECKED_API + 1, Some(CHECKED_API)), Compatibility::CommandDiffers);
        assert!(Compatibility::Checked.warning().is_none() && Compatibility::Newer.warning().is_none());
        assert!(Compatibility::Older.warning().is_some() && Compatibility::CommandDiffers.warning().is_some());
    }

    #[test]
    fn doctor_names_the_host_its_rex_and_where_it_is_seen_from() {
        let mut out = String::new();
        host(CHECKED_API, Some(CHECKED_API), Viewers::Elsewhere).report(&mut out);
        assert_eq!(
            out,
            format!(
                "Host: studio (Rex host d4744373)\n\
                 Rex server: 0.1.0, API {CHECKED_API}; rex command: the same\n\
                 Rex compatibility: checked with API {CHECKED_API}. This is the Rex API Milo was checked with.\n\
                 Rex apps: connected from another device only; on that computer, milo web --host LABEL opens these settings\n"
            )
        );
        let mut without = String::new();
        Host { name: "studio".into(), rex: None, forwarded: false }.report(&mut without);
        assert_eq!(without, "Host: studio\nRex server: none answered; the panes work without it\n");
    }

    #[test]
    fn the_page_gets_a_short_host_id_and_whether_to_warn() {
        let mut out = String::new();
        let mut older = host(CHECKED_API - 1, Some(CHECKED_API - 1), Viewers::Here);
        older.forwarded = true;
        older.json(&mut out);
        assert!(
            out.starts_with(r#"{"name":"studio","forwarded":true,"rex":{"host_id":"d4744373","version":"0.1.0""#),
            "{out}"
        );
        assert!(
            out.contains(r#""compatibility":"older","warning":true"#) && out.ends_with(r#""viewers":"here"}}"#),
            "{out}"
        );
        assert!(crate::json::Value::parse(out.as_bytes()).is_some(), "{out}");
        assert!(!out.contains("81e8"), "the whole host ID stays out of the page: {out}");
    }

    #[test]
    fn a_computer_name_is_one_plain_label() {
        assert_eq!(short_name(b"studio.example.com"), "studio");
        assert_eq!(short_name(b"build\x1b[31m-box"), "build31m-box");
        assert_eq!(short_name(b""), "this computer");
        assert_eq!(short_name(&[b'a'; 200]).len(), 63);
    }
}
