//! Crate-wide error type.

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_json::Error),
    /// The Sway IPC connection was closed or replied with garbage
    /// (the Python original raises `ConnectionError` for this).
    Disconnected(String),
    /// Well-formed but unexpected data (bad state file entry, bad event, ...).
    Protocol(String),
    /// Anything else that makes a command fail (missing dependency, refused request, ...).
    Runtime(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn runtime(msg: impl Into<String>) -> Self {
        Error::Runtime(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Json(e) => write!(f, "invalid JSON: {e}"),
            Error::Disconnected(m) | Error::Protocol(m) | Error::Runtime(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}
