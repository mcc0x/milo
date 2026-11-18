//! Asking Rex, the terminal this may be running inside, to open a floating
//! window. Everything goes through the `rex` command line.

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs the Rex command line and returns what it printed. `None` if it failed.
fn rex(args: &[&str]) -> Option<String> {
    let out = Command::new("rex").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
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

/// The value of a string field, wherever it first appears in a JSON reply.
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

/// Opens `file` in a Reader, in a floating window over the tab
/// this pane is in, closing the window `previous` first if there is one.
/// Returns the new window's name for the next call, or what to tell the
/// user when it could not be done.
pub fn open(root: &Path, file: &str, previous: Option<&str>) -> Result<String, &'static str> {
    let session = std::env::var("REX_SESSION").map_err(|_| "Reading a file needs Rex.")?;
    let exe = std::env::current_exe().map_err(|_| "Rex did not open the file.")?;
    if let Some(layer) = previous {
        // It may be closed already; either way it is gone afterwards.
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
    let name = file.rsplit_once('/').map_or(file, |(_, n)| n);
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
