//! Speaks Discord's local RPC protocol to the Discord app on this computer.
//!
//! Discord documents the socket, the framing and the commands at
//! <https://docs.discord.com/developers/topics/rpc>. A frame is an opcode and
//! a length, each four bytes with the low byte first, then that many bytes
//! of JSON. Setting an activity needs the public ID of a Discord application
//! and nothing else: no token, secret or sign-in.
//!
//! The activity is one fixed line. A second line is added only when the
//! person chose to show the session or project name, and holds only what
//! `label` made of those names. Nothing else is ever put in a frame.

use std::ffi::OsString;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::json::Value;

/// The one line of text shown under the application's name.
pub(crate) const DETAILS: &str = "In a Rex session";

const HANDSHAKE: u32 = 0;
const FRAME: u32 = 1;
const CLOSE: u32 = 2;
const PING: u32 = 3;
const PONG: u32 = 4;
/// Discord closes a connection made with an application ID it does not know.
const INVALID_CLIENT_ID: u64 = 4000;
/// Discord's replies are a few hundred bytes; more is not a reply to wait for.
const PAYLOAD_LIMIT: usize = 1 << 16;
/// How long Discord is given to answer the handshake.
const REPLY_WAIT: Duration = Duration::from_secs(2);
/// Discord listens on the first of these it finds free.
const SOCKETS: u32 = 10;

/// Why there is no connection to set an activity on.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure {
    /// No Discord app answered, as when it is not running or not installed.
    Missing,
    /// Discord does not know the application ID. Asking again will not help.
    UnknownApplication,
}

/// Why an open connection is over.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ended {
    /// Discord closed the connection or went away.
    Lost,
    /// Discord answered a request with an error. Sending it again will not help.
    Rejected,
}

/// The folder Discord puts its socket in. Discord uses the first of these
/// that is set, so Milo looks in that one too and nowhere else.
pub(super) fn socket_dir(variable: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .into_iter()
        .find_map(|name| variable(name).filter(|value| !value.is_empty()))
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
}

pub(super) struct Connection {
    stream: UnixStream,
    /// Bytes received that do not yet make a whole frame.
    received: Vec<u8>,
    process: u32,
    requests: u64,
}

impl Connection {
    /// Connects to the Discord app whose socket is in `dir` and introduces
    /// the application. Nothing is shown until `set_activity`.
    pub(super) fn open(dir: &Path, application: u64) -> Result<Self, Failure> {
        for number in 0..SOCKETS {
            // A helper asked to stop does not go on to the next socket.
            if crate::term::TERMINATED.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let Ok(stream) = UnixStream::connect(dir.join(format!("discord-ipc-{number}"))) else { continue };
            match Self::introduce(stream, application) {
                Ok(connection) => return Ok(connection),
                Err(Some(failure)) => return Err(failure),
                // A socket left behind by a Discord that has gone. Try the next.
                Err(None) => {}
            }
        }
        Err(Failure::Missing)
    }

    fn introduce(mut stream: UnixStream, application: u64) -> Result<Self, Option<Failure>> {
        let timed = stream.set_read_timeout(Some(REPLY_WAIT)).and_then(|()| stream.set_write_timeout(Some(REPLY_WAIT)));
        timed.map_err(|_| None)?;
        send(&mut stream, HANDSHAKE, &handshake(application)).map_err(|_| None)?;
        let (opcode, payload) = read_frame(&mut stream).map_err(|_| None)?;
        let reply = Value::parse(&payload);
        match opcode {
            FRAME if reply.is_some_and(|reply| event_is(reply, "READY")) => {}
            CLOSE if reply.and_then(close_code) == Some(INVALID_CLIENT_ID) => {
                return Err(Some(Failure::UnknownApplication));
            }
            _ => return Err(None),
        }
        // From here on, Discord is only read when it has something to say.
        stream.set_nonblocking(true).map_err(|_| None)?;
        Ok(Self { stream, received: Vec::new(), process: std::process::id(), requests: 0 })
    }

    pub(super) fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Shows the activity on the user's Discord profile, with `names` as
    /// its second line when the person chose to show any.
    pub(super) fn set_activity(&mut self, names: Option<&str>) -> io::Result<()> {
        self.requests += 1;
        send(&mut self.stream, FRAME, &activity(self.process, self.requests, names))
    }

    /// Takes the activity down and ends the connection. Discord also drops
    /// the activity when the connection closes, so a failure here is not kept.
    pub(super) fn clear(mut self) {
        self.requests += 1;
        let _ = send(&mut self.stream, FRAME, &no_activity(self.process, self.requests));
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    /// Takes in what Discord has sent, answering a ping. Call it when the
    /// connection is readable.
    pub(super) fn receive(&mut self) -> Result<(), Ended> {
        let mut bytes = [0; 4096];
        loop {
            match self.stream.read(&mut bytes) {
                Ok(0) => return Err(Ended::Lost),
                Ok(n) => self.received.extend_from_slice(&bytes[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => return Err(Ended::Lost),
            }
            while let Some((opcode, payload)) = self.next_frame()? {
                match opcode {
                    PING => send(&mut self.stream, PONG, &payload).map_err(|_| Ended::Lost)?,
                    CLOSE => return Err(Ended::Lost),
                    FRAME if Value::parse(payload.as_bytes()).is_some_and(|reply| event_is(reply, "ERROR")) => {
                        return Err(Ended::Rejected);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// The next whole frame received, as its opcode and JSON text.
    fn next_frame(&mut self) -> Result<Option<(u32, String)>, Ended> {
        let Some((opcode, length)) = header(&self.received) else { return Ok(None) };
        if length > PAYLOAD_LIMIT {
            return Err(Ended::Lost);
        }
        if self.received.len() < 8 + length {
            return Ok(None);
        }
        let payload = String::from_utf8_lossy(&self.received[8..8 + length]).into_owned();
        self.received.drain(..8 + length);
        Ok(Some((opcode, payload)))
    }
}

fn header(bytes: &[u8]) -> Option<(u32, usize)> {
    let (opcode, rest) = bytes.split_first_chunk::<4>()?;
    let (length, _) = rest.split_first_chunk::<4>()?;
    Some((u32::from_le_bytes(*opcode), usize::try_from(u32::from_le_bytes(*length)).ok()?))
}

fn send(stream: &mut UnixStream, opcode: u32, payload: &str) -> io::Result<()> {
    let length = u32::try_from(payload.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&opcode.to_le_bytes());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(payload.as_bytes());
    // One write, so that a frame is never left half sent on a full socket.
    match stream.write(&frame) {
        Ok(sent) if sent == frame.len() => Ok(()),
        Ok(_) => Err(io::ErrorKind::WriteZero.into()),
        Err(error) => Err(error),
    }
}

fn read_frame(stream: &mut UnixStream) -> io::Result<(u32, Vec<u8>)> {
    let mut start = [0; 8];
    read_whole(stream, &mut start)?;
    let (opcode, length) = header(&start).ok_or(io::ErrorKind::InvalidData)?;
    if length > PAYLOAD_LIMIT {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut payload = vec![0; length];
    read_whole(stream, &mut payload)?;
    Ok((opcode, payload))
}

/// Fills `bytes`, giving up when a signal arrives, so that a helper asked
/// to stop is not kept waiting on a Discord that does not answer.
fn read_whole(stream: &mut UnixStream, mut bytes: &mut [u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match stream.read(bytes)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => bytes = &mut bytes[n..],
        }
    }
    Ok(())
}

fn event_is(reply: Value<'_>, name: &str) -> bool {
    let [event] = reply.fields(["evt"]);
    event.is_some_and(|event| event.is(name))
}

fn close_code(reply: Value<'_>) -> Option<u64> {
    let [code] = reply.fields(["code"]);
    code?.unsigned()
}

fn handshake(application: u64) -> String {
    format!(r#"{{"v":1,"client_id":"{application}"}}"#)
}

/// Everything Milo ever tells Discord to show: the fixed line, and the names the person chose to add.
fn activity(process: u32, request: u64, names: Option<&str>) -> String {
    let state = names.map(|names| format!(r#","state":{}"#, quoted(names))).unwrap_or_default();
    format!(
        r#"{{"cmd":"SET_ACTIVITY","args":{{"pid":{process},"activity":{{"details":"{DETAILS}"{state}}}}},"nonce":"milo-{request}"}}"#
    )
}

/// Text as a JSON string.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' | '\\' => out.extend(['\\', c]),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The same command with no activity, which takes the activity down.
fn no_activity(process: u32, request: u64) -> String {
    format!(r#"{{"cmd":"SET_ACTIVITY","args":{{"pid":{process}}},"nonce":"milo-{request}"}}"#)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_folder_is_the_first_one_discord_would_use() {
        let set = |pairs: &'static [(&str, &str)]| {
            move |name: &str| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| OsString::from(value))
        };
        assert_eq!(socket_dir(set(&[("TMPDIR", "/t"), ("XDG_RUNTIME_DIR", "/run/user/1")])), Path::new("/run/user/1"));
        assert_eq!(socket_dir(set(&[("TEMP", "/c"), ("TMP", "/b"), ("TMPDIR", "/a")])), Path::new("/a"));
        assert_eq!(socket_dir(set(&[("XDG_RUNTIME_DIR", ""), ("TEMP", "/c")])), Path::new("/c"));
        assert_eq!(socket_dir(set(&[])), Path::new("/tmp"));
    }

    #[test]
    fn the_activity_holds_one_fixed_line_and_nothing_about_the_work() {
        assert_eq!(handshake(1_234_567_890_123_456_789), r#"{"v":1,"client_id":"1234567890123456789"}"#);
        assert_eq!(
            activity(42, 1, None),
            r#"{"cmd":"SET_ACTIVITY","args":{"pid":42,"activity":{"details":"In a Rex session"}},"nonce":"milo-1"}"#
        );
        // Chosen names are a second line, written so that no character can end the text early.
        assert_eq!(
            activity(42, 3, Some("Project: milo · Session: say \"hi\" \\ \u{1}")),
            r#"{"cmd":"SET_ACTIVITY","args":{"pid":42,"activity":{"details":"In a Rex session","state":"Project: milo · Session: say \"hi\" \\ \u0001"}},"nonce":"milo-3"}"#
        );
        assert_eq!(no_activity(42, 2), r#"{"cmd":"SET_ACTIVITY","args":{"pid":42},"nonce":"milo-2"}"#);
    }

    #[test]
    fn frames_arriving_in_pieces_are_answered_once_whole() {
        let (mut discord, ours) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let mut connection = Connection { stream: ours, received: Vec::new(), process: 7, requests: 0 };
        let mut ping = Vec::new();
        ping.extend_from_slice(&PING.to_le_bytes());
        ping.extend_from_slice(&4u32.to_le_bytes());
        ping.extend_from_slice(b"\"hi\"");
        discord.write_all(&ping[..5]).unwrap();
        assert_eq!(connection.receive(), Ok(()));
        discord.write_all(&ping[5..]).unwrap();
        assert_eq!(connection.receive(), Ok(()));
        assert_eq!(read_frame(&mut discord).unwrap(), (PONG, b"\"hi\"".to_vec()));

        // A frame too large to be a reply ends the connection rather than filling memory.
        let mut huge = FRAME.to_le_bytes().to_vec();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        discord.write_all(&huge).unwrap();
        assert_eq!(connection.receive(), Err(Ended::Lost));
    }

    #[test]
    fn an_error_reply_and_a_closed_connection_end_it_differently() {
        for (opcode, payload, ended) in [
            (FRAME, r#"{"cmd":"SET_ACTIVITY","evt":"ERROR","data":{"code":4000}}"#, Ended::Rejected),
            (CLOSE, r#"{"code":1000,"message":"bye"}"#, Ended::Lost),
        ] {
            let (mut discord, ours) = UnixStream::pair().unwrap();
            ours.set_nonblocking(true).unwrap();
            let mut connection = Connection { stream: ours, received: Vec::new(), process: 7, requests: 0 };
            send(&mut discord, FRAME, r#"{"cmd":"SET_ACTIVITY","evt":null,"data":{}}"#).unwrap();
            assert_eq!(connection.receive(), Ok(()));
            send(&mut discord, opcode, payload).unwrap();
            assert_eq!(connection.receive(), Err(ended));
        }
        let (discord, ours) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        drop(discord);
        let mut connection = Connection { stream: ours, received: Vec::new(), process: 7, requests: 0 };
        assert_eq!(connection.receive(), Err(Ended::Lost));
    }
}
