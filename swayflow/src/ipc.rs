//! Minimal synchronous client for the i3/Sway IPC protocol.
//!
//! Wire format: `"i3-ipc"` magic, `u32` payload length, `u32` message type
//! (both in native byte order), followed by the payload.

use std::env;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::tree::Node;

const IPC_MAGIC: &[u8; 6] = b"i3-ipc";
const HEADER_LEN: usize = 14;
/// Sanity limit so that a corrupt header cannot make us allocate gigabytes.
const MAX_PAYLOAD: usize = 64 * 1024 * 1024;

pub const IPC_RUN_COMMAND: u32 = 0;
pub const IPC_SUBSCRIBE: u32 = 2;
pub const IPC_GET_TREE: u32 = 4;
pub const EVENT_WORKSPACE: u32 = 0x8000_0000;
pub const EVENT_WINDOW: u32 = 0x8000_0003;

pub struct Sway {
    path: String,
    stream: UnixStream,
}

fn socket_path_from_sway() -> Result<String> {
    let output = Command::new("sway").arg("--get-socketpath").output()?;
    if !output.status.success() {
        return Err(Error::runtime("`sway --get-socketpath` failed"));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if path.is_empty() {
        return Err(Error::runtime("`sway --get-socketpath` returned nothing"));
    }
    Ok(path)
}

/// Connection-level failures become `Error::Disconnected`
/// (Python: subclasses of `ConnectionError`), everything else stays an I/O error.
fn map_io(err: io::Error) -> Error {
    match err.kind() {
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::NotConnected => {
            Error::Disconnected(format!("sway IPC closed the connection: {err}"))
        }
        _ => Error::Io(err),
    }
}

impl Sway {
    /// Connects to `$SWAYSOCK`, falling back to `sway --get-socketpath`.
    pub fn connect() -> Result<Sway> {
        let path = match env::var("SWAYSOCK") {
            Ok(p) if !p.is_empty() => p,
            _ => socket_path_from_sway()?,
        };
        let stream = UnixStream::connect(&path)?;
        Ok(Sway { path, stream })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Reads one message (reply or event) and returns its type and raw payload.
    pub fn read_message_raw(&mut self) -> Result<(u32, Vec<u8>)> {
        let mut header = [0u8; HEADER_LEN];
        self.stream.read_exact(&mut header).map_err(map_io)?;
        if &header[..6] != IPC_MAGIC {
            return Err(Error::Disconnected("unexpected sway IPC reply".into()));
        }
        let length = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]) as usize;
        let mtype = u32::from_ne_bytes([header[10], header[11], header[12], header[13]]);
        if length > MAX_PAYLOAD {
            return Err(Error::Protocol(format!("sway IPC payload too large: {length} bytes")));
        }
        let mut body = vec![0u8; length];
        self.stream.read_exact(&mut body).map_err(map_io)?;
        Ok((mtype, body))
    }

    pub fn read_message(&mut self) -> Result<(u32, Value)> {
        let (mtype, body) = self.read_message_raw()?;
        Ok((mtype, serde_json::from_slice(&body)?))
    }

    fn request_raw(&mut self, mtype: u32, payload: &str) -> Result<Vec<u8>> {
        let data = payload.as_bytes();
        let length = u32::try_from(data.len())
            .map_err(|_| Error::Protocol("sway IPC request too large".into()))?;
        let mut msg = Vec::with_capacity(HEADER_LEN + data.len());
        msg.extend_from_slice(IPC_MAGIC);
        msg.extend_from_slice(&length.to_ne_bytes());
        msg.extend_from_slice(&mtype.to_ne_bytes());
        msg.extend_from_slice(data);
        self.stream.write_all(&msg).map_err(map_io)?;
        // Like the original, the next message on this connection is taken as the reply.
        Ok(self.read_message_raw()?.1)
    }

    pub fn request(&mut self, mtype: u32, payload: &str) -> Result<Value> {
        Ok(serde_json::from_slice(&self.request_raw(mtype, payload)?)?)
    }

    pub fn tree(&mut self) -> Result<Node> {
        Ok(serde_json::from_slice(&self.request_raw(IPC_GET_TREE, "")?)?)
    }

    /// Runs a (possibly `;`-chained) Sway command. An empty command is a no-op.
    pub fn run(&mut self, command: &str) -> Result<Value> {
        if command.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        self.request(IPC_RUN_COMMAND, command)
    }

    pub fn subscribe(&mut self, events: &[&str]) -> Result<()> {
        let payload = serde_json::to_string(events)?;
        let reply = self.request(IPC_SUBSCRIBE, &payload)?;
        if reply.get("success").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(Error::runtime("sway refused the event subscription"))
        }
    }
}
