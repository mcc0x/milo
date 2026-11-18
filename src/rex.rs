//! Calls Rex for floating readers, setup, and the new-tab listener.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

fn rex(args: &[&str]) -> Option<String> {
    let out = Command::new("rex").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The shell that opened setup, even if focus has moved somewhere else.
pub struct Target {
    pub session: OsString,
    pub block: OsString,
}

impl Target {
    pub fn current() -> Option<Self> {
        Some(Self { session: std::env::var_os("REX_SESSION")?, block: std::env::var_os("REX_BLOCK")? })
    }
}

/// Setup reports its own messages, not process output that can contain paths or settings.
pub fn run(args: &[&OsStr]) -> bool {
    let mut command = Command::new("rex");
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if args.starts_with(&[OsStr::new("config"), OsStr::new("check")]) {
        command.env("MILO_CHECKING_REX", "1");
    }
    command.status().is_ok_and(|status| status.success())
}

/// Replaces milo without releasing the listener's inherited lock.
pub fn exec_listener(script: &Path) -> io::Error {
    Command::new("rex")
        .arg("do")
        .arg(script)
        .arg("--all")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .exec()
}

/// A string as a JSON literal.
fn json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => out.extend(['\\', c]),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Extracts a simple ID such as `layer:1` from a Rex reply.
fn field(reply: &str, name: &str) -> Option<String> {
    let (_, rest) = reply.split_once(&format!("\"{name}\""))?;
    let value = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    Some(value.split_once('"')?.0.to_string())
}

/// Whether this is running in a Rex pane, where a file can be opened in a
/// floating window.
pub fn inside() -> bool {
    std::env::var_os("REX_SESSION").is_some()
}

/// Opens a floating reader in the outline's tab and returns its layer ID.
/// If a previous layer is known, try to close it first.
pub fn open(root: &Path, file: &str, previous: Option<&str>) -> Result<String, &'static str> {
    let session = std::env::var("REX_SESSION").map_err(|_| "Reading a file needs Rex.")?;
    let exe = std::env::current_exe().map_err(|_| "Rex did not open the file.")?;
    if let Some(layer) = previous {
        // The user may already have closed the layer; still try to open the file.
        let request = format!(r#"{{"session_id":{},"layer_id":{}}}"#, json(&session), json(layer));
        rex(&["api", "call", "-s", &session, "session.close_layer", &request]);
    }
    // Over the tab the outline is in, even if the user's focus is elsewhere;
    // failing that, over the active one.
    let window = std::env::var("REX_BLOCK")
        .ok()
        .and_then(|block| rex(&["block", "inspect", "-s", &session, &block, "--json"]))
        .and_then(|reply| field(&reply, "window_id"))
        .map(|id| format!(r#""window_id":{},"#, json(&id)))
        .unwrap_or_default();
    let (_, name) = file.rsplit_once('/').unwrap_or(("", file));
    let request = format!(
        concat!(
            r#"{{"session_id":{},{}"bounds":[0.12,0.08,0.76,0.84],"focus":true,"layout":{{"block":{{"#,
            r#""flavor":"com.superlogical.terminal.shell","label":{},"#,
            r#""options":{{"command":[{},"view",{}],"shell":"none","cwd":{}}}}}}}}}"#,
        ),
        json(&session),
        window,
        json(name),
        json(&exe.to_string_lossy()),
        json(&root.join(file).to_string_lossy()),
        json(&root.to_string_lossy()),
    );
    rex(&["api", "call", "-s", &session, "session.new_layer", &request])
        .and_then(|reply| field(&reply, "layer_id"))
        .ok_or("Rex did not open the file.")
}

#[cfg(test)]
mod tests {
    use super::{field, json};

    #[test]
    fn strings_are_escaped_for_json() {
        assert_eq!(json(r#"a "b" \c"#), r#""a \"b\" \\c""#);
        assert_eq!(json("tab\there"), "\"tab\\u0009here\"");
    }

    #[test]
    fn a_field_is_found_compact_or_spaced() {
        assert_eq!(field(r#"{"layer_id":"layer:1","n":2}"#, "layer_id").as_deref(), Some("layer:1"));
        assert_eq!(field("{\n  \"window_id\": \"window:9\"\n}", "window_id").as_deref(), Some("window:9"));
        assert_eq!(field(r#"{"other":"x"}"#, "layer_id"), None);
    }
}
