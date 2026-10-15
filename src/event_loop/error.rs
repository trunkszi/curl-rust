use crate::easy::Easy;
use crate::{Error as CurlError, MultiError};
use std::fmt;

#[derive(Debug)]
/// Public event-loop value or operation.
pub enum Error {
    /// libcurl reported an error.
    Curl(CurlError),
    /// The multi interface reported an error.
    Multi(MultiError),
    /// Operating system I/O failed.
    Io(std::io::Error),
    /// The loop was re-entered.
    Reentrant,
    /// The requested token or watch was not found.
    NotFound,
    /// The file descriptor is already registered in the event loop.
    AlreadyRegistered,
    /// The owning loop was dropped.
    LoopGone,
    /// A host lacked an explicit address.
    UnresolvedHost,
    /// The outbound buffer is full.
    SendBufferFull,
    /// A message exceeded its configured limit.
    MessageTooLarge,
    /// A close handshake timed out.
    CloseTimeout,
    /// A text message was not UTF-8.
    InvalidUtf8,
    /// The protocol handshake failed.
    HandshakeFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Curl(e) => write!(f, "curl error: {e}"),
            Self::Multi(e) => write!(f, "multi error: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Reentrant => write!(f, "event loop re-entered during callback"),
            Self::NotFound => write!(f, "token or watch not found in event loop"),
            Self::AlreadyRegistered => write!(f, "file descriptor already registered in event loop"),
            Self::LoopGone => write!(f, "owning event loop was dropped"),
            Self::UnresolvedHost => write!(f, "target or proxy host was not pre-resolved"),
            Self::SendBufferFull => write!(f, "websocket outbound send buffer is full"),
            Self::MessageTooLarge => write!(f, "websocket message exceeded max_message_size"),
            Self::CloseTimeout => write!(f, "websocket close handshake timed out"),
            Self::InvalidUtf8 => write!(f, "websocket text frame or close reason is not valid UTF-8"),
            Self::HandshakeFailed => write!(f, "websocket handshake failed to produce an active socket"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Curl(e) => Some(e),
            Self::Multi(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Debug)]
/// Public event-loop value or operation.
pub struct AddError {
    /// The original easy handle.
    pub easy: Easy,
    /// The error that prevented registration.
    pub error: Error,
}

impl fmt::Display for AddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.error, f)
    }
}

use std::fmt::Display;

impl std::error::Error for AddError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl From<CurlError> for Error {
    fn from(e: CurlError) -> Self {
        Self::Curl(e)
    }
}

impl From<MultiError> for Error {
    fn from(e: MultiError) -> Self {
        Self::Multi(e)
    }
}

