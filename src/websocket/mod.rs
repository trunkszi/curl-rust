//! Pull-mode WebSocketClient message types.
/// Public interface for this event-loop operation.
pub(crate) mod connection;
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::{Rc, Weak};

#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = "WebSocket public interface."]
pub enum Message<'a> {
    #[doc = "WebSocket message variant."]
    Text(&'a str),
    #[doc = "WebSocket message variant."]
    Binary(&'a [u8]),
    #[doc = "WebSocket message variant."]
    Ping(&'a [u8]),
    #[doc = "WebSocket message variant."]
    Pong(&'a [u8]),
    #[doc = "WebSocket message variant."]
    Close {
        /// Close status code.
        code: CloseCode,
        /// Human-readable close reason.
        reason: &'a str,
    },
}

/// Public interface for this event-loop operation.
#[doc = "WebSocket public interface."]
pub trait WebSocketHandler {
    /// Handles this WebSocket operation.
    fn on_open(&mut self, _ws: &WebSocketClient) {}
    /// Handles this WebSocket operation.
    fn on_message(&mut self, _ws: &WebSocketClient, _message: Message<'_>) {}
    /// Handles this WebSocket operation.
    fn on_close(&mut self, _ws: &WebSocketClient, _code: CloseCode, _reason: &str) {}
    /// Handles this WebSocket operation.
    fn on_error(&mut self, _ws: &WebSocketClient, _error: crate::event_loop::Error) {}
}

#[derive(Debug, Clone)]
#[doc = "WebSocket public interface."]
pub struct Options {
    /// Public interface for this event-loop operation.
    pub max_message_size: usize,
    /// Maximum queued payload bytes and maximum payload size accepted by `send`.
    /// Oversized messages are rejected before any bytes are transmitted.
    pub max_send_buffer: usize,
    /// Public interface for this event-loop operation.
    pub close_timeout: std::time::Duration,
    /// Public interface for this event-loop operation.
    pub ping_interval: Option<std::time::Duration>,
    /// Public interface for this event-loop operation.
    pub idle_timeout: Option<std::time::Duration>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_message_size: 16 * 1024 * 1024,
            max_send_buffer: 4 * 1024 * 1024,
            close_timeout: std::time::Duration::from_secs(5),
            ping_interval: None,
            idle_timeout: None,
        }
    }
}

impl Options {
    /// Public interface for this event-loop operation.
    pub fn max_message_size(mut self, n: usize) -> Self {
        self.max_message_size = n;
        self
    }
    /// Public interface for this event-loop operation.
    pub fn max_send_buffer(mut self, n: usize) -> Self {
        self.max_send_buffer = n;
        self
    }
    /// Public interface for this event-loop operation.
    pub fn close_timeout(mut self, d: std::time::Duration) -> Self {
        self.close_timeout = d;
        self
    }
    /// Public interface for this event-loop operation.
    pub fn ping_interval(mut self, d: Option<std::time::Duration>) -> Self {
        self.ping_interval = d;
        self
    }
    /// Public interface for this event-loop operation.
    pub fn idle_timeout(mut self, d: Option<std::time::Duration>) -> Self {
        self.idle_timeout = d;
        self
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[doc = "WebSocket public interface."]
pub struct CloseCode(pub u16);
impl CloseCode {
    /// Normal closure (`1000`).
    pub const NORMAL: Self = Self(1000);
    /// Endpoint is going away (`1001`).
    pub const GOING_AWAY: Self = Self(1001);
    /// Protocol error (`1002`).
    pub const PROTOCOL_ERROR: Self = Self(1002);
    /// Unsupported data (`1003`).
    pub const UNSUPPORTED_DATA: Self = Self(1003);
    /// No status code was present (`1005`).
    pub const NO_STATUS_RECEIVED: Self = Self(1005);
    /// Abnormal closure without Close frame (`1006`).
    pub const ABNORMAL_CLOSURE: Self = Self(1006);
    /// Invalid frame payload data such as non-UTF-8 text (`1007`).
    pub const INVALID_PAYLOAD: Self = Self(1007);
    /// Message is too big to process (`1009`).
    pub const MESSAGE_TOO_BIG: Self = Self(1009);
}

#[doc = "WebSocket public interface."]
pub struct WebSocketClient {
    _marker: PhantomData<*mut ()>,
    pub(crate) loop_: Weak<crate::event_loop::LoopInner>,
    pub(crate) token: crate::event_loop::Token,
    pub(crate) client_count: Rc<Cell<usize>>,
    pub(crate) is_callback_ref: bool,
}

impl Clone for WebSocketClient {
    fn clone(&self) -> Self {
        self.client_count.set(self.client_count.get() + 1);
        Self {
            _marker: PhantomData,
            loop_: self.loop_.clone(),
            token: self.token,
            client_count: self.client_count.clone(),
            is_callback_ref: false,
        }
    }
}

impl Drop for WebSocketClient {
    fn drop(&mut self) {
        if self.is_callback_ref {
            return;
        }
        let prev = self.client_count.get();
        let next = prev.saturating_sub(1);
        self.client_count.set(next);
        if next == 0 {
            if let Some(loop_) = self.loop_.upgrade() {
                loop_.on_ws_clients_zero(self.token);
            }
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[doc = "WebSocket public interface."]
pub enum WebSocketState {
    /// Handles this WebSocket operation.
    Connecting,
    /// Handles this WebSocket operation.
    Open,
    /// Handles this WebSocket operation.
    Closing,
    /// Handles this WebSocket operation.
    Closed,
}

impl std::fmt::Debug for WebSocketClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketClient")
            .field("token", &self.token)
            .finish()
    }
}

impl WebSocketClient {
    /// Starts a WebSocket connection on an event loop and returns a client handle immediately.
    pub fn connect<H: WebSocketHandler + 'static>(
        loop_: &crate::event_loop::EventLoop,
        easy: crate::easy::Easy,
        options: Options,
        handler: H,
    ) -> Result<Self, crate::event_loop::AddError> {
        let mut easy = easy;
        if let Err(error) = easy.websocket_connect_only() {
            return Err(crate::event_loop::AddError {
                easy,
                error: error.into(),
            });
        }
        let client_count = Rc::new(Cell::new(1usize));
        let token = loop_.inner.enqueue_websocket(
            easy,
            options,
            Box::new(handler),
            client_count.clone(),
        )?;
        Ok(Self {
            _marker: PhantomData,
            loop_: Rc::downgrade(&loop_.inner),
            token,
            client_count,
            is_callback_ref: false,
        })
    }

    pub(crate) fn callback_ref(
        loop_: Weak<crate::event_loop::LoopInner>,
        token: crate::event_loop::Token,
        client_count: Rc<Cell<usize>>,
    ) -> Self {
        Self {
            _marker: PhantomData,
            loop_,
            token,
            client_count,
            is_callback_ref: true,
        }
    }

    /// Sends a WebSocket message, writing directly to the socket when the connection is open and unbuffered.
    pub fn send(&self, message: Message<'_>) -> Result<(), crate::event_loop::Error> {
        if let Message::Close { code, reason } = message {
            return self.close(code, reason);
        }
        let loop_ = self
            .loop_
            .upgrade()
            .ok_or(crate::event_loop::Error::LoopGone)?;
        loop_.ws_send(self.token, message)
    }

    /// Sends a UTF-8 text message without allocating an intermediate `String`.
    pub fn send_text(&self, text: &str) -> Result<(), crate::event_loop::Error> {
        self.send(Message::Text(text))
    }

    /// Sends a binary message without allocating an intermediate `Vec<u8>`.
    pub fn send_binary(&self, data: &[u8]) -> Result<(), crate::event_loop::Error> {
        self.send(Message::Binary(data))
    }

    /// Initiates a graceful WebSocket close handshake.
    pub fn close(&self, code: CloseCode, reason: &str) -> Result<(), crate::event_loop::Error> {
        let loop_ = self
            .loop_
            .upgrade()
            .ok_or(crate::event_loop::Error::LoopGone)?;
        loop_.ws_close(self.token, code, reason)
    }

    /// Returns the current protocol state.
    pub fn state(&self) -> WebSocketState {
        self.loop_
            .upgrade()
            .map_or(WebSocketState::Closed, |loop_| loop_.ws_state(self.token))
    }

    /// Returns the underlying socket when the handshake is complete.
    pub fn raw_fd(&self) -> Option<i32> {
        self.loop_
            .upgrade()
            .and_then(|loop_| loop_.ws_raw_fd(self.token))
    }
}

