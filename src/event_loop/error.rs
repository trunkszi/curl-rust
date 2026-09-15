use crate::easy::Easy;
use crate::{Error as CurlError, MultiError};

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

#[derive(Debug)]
/// Public event-loop value or operation.
pub struct AddError {
    /// The original easy handle.
    pub easy: Easy,
    /// The error that prevented registration.
    pub error: Error,
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
