//! Pull-mode WebSocketClient message types.
/// Public interface for this event-loop operation.
pub(crate) mod connection;
/// Public interface for this event-loop operation.
pub(crate) mod runtime;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::{Rc, Weak};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = "WebSocket public interface."]
pub enum Message {
    #[doc = "WebSocket message variant."]
    Text(String),
    #[doc = "WebSocket message variant."]
    Binary(Vec<u8>),
    #[doc = "WebSocket message variant."]
    Ping(Vec<u8>),
    #[doc = "WebSocket message variant."]
    Pong(Vec<u8>),
    #[doc = "WebSocket message variant."]
    Close {
        /// Close status code.
        code: CloseCode,
        /// Human-readable close reason.
        reason: String,
    },
}

/// Public interface for this event-loop operation.
#[doc = "WebSocket public interface."]
pub trait WebSocketHandler {
    /// Handles this WebSocket operation.
    fn on_open(&mut self, _ws: &WebSocketClient) {}
    /// Handles this WebSocket operation.
    fn on_message(&mut self, _ws: &WebSocketClient, _message: Message) {}
    /// Handles this WebSocket operation.
    fn on_close(&mut self, _ws: &WebSocketClient, _code: CloseCode, _reason: String) {}
    /// Handles this WebSocket operation.
    fn on_error(&mut self, _ws: &WebSocketClient, _error: crate::event_loop::Error) {}
}

#[derive(Debug, Clone)]
#[doc = "WebSocket public interface."]
pub struct Options {
    /// Public interface for this event-loop operation.
    pub max_message_size: usize,
    /// Public interface for this event-loop operation.
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
    /// Public interface for this event-loop operation.
    pub const NORMAL: Self = Self(1000);
    /// Public interface for this event-loop operation.
    pub const GOING_AWAY: Self = Self(1001);
    /// Public interface for this event-loop operation.
    pub const PROTOCOL_ERROR: Self = Self(1002);
    /// Public interface for this event-loop operation.
    pub const UNSUPPORTED_DATA: Self = Self(1003);
}

#[derive(Clone)]
#[doc = "WebSocket public interface."]
pub struct WebSocketClient {
    _marker: PhantomData<*mut ()>,
    /// Public interface for this event-loop operation.
    pub(crate) shared: Rc<RefCell<WebSocketShared>>,
    loop_: Weak<crate::event_loop::LoopInner>,
    guard: Rc<()>,
}

impl Drop for WebSocketClient {
    fn drop(&mut self) {
        if Rc::strong_count(&self.guard) != 1 {
            return;
        }
        let mut state = self.shared.borrow_mut();
        if state.state == WebSocketState::Open {
            let _ = state.protocol.close(CloseCode::NORMAL, "", Instant::now());
            state.state = WebSocketState::Closing;
        }
    }
}

/// Public interface for this event-loop operation.
pub(crate) struct WebSocketShared {
    /// Public interface for this event-loop operation.
    pub(crate) protocol: connection::Connection,
    /// Public interface for this event-loop operation.
    pub(crate) curl_socket: Option<i32>,
    /// Public interface for this event-loop operation.
    pub message_cb: Option<Box<dyn FnMut(Message)>>,
    /// Public interface for this event-loop operation.
    pub close_cb: Option<Box<dyn FnMut(CloseCode, String)>>,
    /// Public interface for this event-loop operation.
    pub error_cb: Option<Box<dyn FnMut(crate::event_loop::Error)>>,
    /// Public interface for this event-loop operation.
    pub state: WebSocketState,
    handler: Option<Box<dyn WebSocketHandler>>,
    loop_: Weak<crate::event_loop::LoopInner>,
    guard: Weak<()>,
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
        f.debug_struct("WebSocketClient").finish()
    }
}

impl WebSocketClient {
    /// Starts a WebSocket connection on an event loop.
    /// Starts a connection and returns a client handle immediately.
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
        let shared = Rc::new(RefCell::new(WebSocketShared::new(options)));
        let client = Self::new(shared.clone(), Rc::downgrade(&loop_.inner), Rc::new(()));
        {
            let mut state = shared.borrow_mut();
            state.handler = Some(Box::new(handler));
            state.loop_ = client.loop_.clone();
            state.guard = Rc::downgrade(&client.guard);
        }
        loop_.add_driver(
            easy,
            Box::new(move |result| match result {
                Ok(fd) => {
                    {
                        let mut state = shared.borrow_mut();
                        state.state = WebSocketState::Open;
                        state.curl_socket = Some(fd);
                    }
                    dispatch(&shared, |handler, ws| handler.on_open(ws));
                    Some(Box::new(runtime::RuntimeDriver {
                        shared,
                        ending: None,
                    }))
                }
                Err(error) => {
                    shared.borrow_mut().state = WebSocketState::Closed;
                    dispatch(&shared, |handler, ws| handler.on_error(ws, error));
                    dispatch(&shared, |handler, ws| {
                        handler.on_close(ws, CloseCode(1006), "connection failed".into())
                    });
                    None
                }
            }),
        )?;
        Ok(client)
    }

    /// Public interface for this event-loop operation.
    pub(crate) fn new(
        shared: Rc<RefCell<WebSocketShared>>,
        loop_: Weak<crate::event_loop::LoopInner>,
        guard: Rc<()>,
    ) -> Self {
        Self {
            shared,
            loop_,
            guard,
            _marker: PhantomData,
        }
    }

    /// Public interface for this event-loop operation.
    pub fn on_message<F>(&mut self, f: F) -> &mut Self
    where
        F: FnMut(Message) + 'static,
    {
        self.shared.borrow_mut().message_cb = Some(Box::new(f));
        self
    }

    /// Public interface for this event-loop operation.
    pub fn on_error<F>(&mut self, f: F) -> &mut Self
    where
        F: FnMut(crate::event_loop::Error) + 'static,
    {
        self.shared.borrow_mut().error_cb = Some(Box::new(f));
        self
    }

    /// Public interface for this event-loop operation.
    pub fn on_close<F>(&mut self, f: F) -> &mut Self
    where
        F: FnMut(CloseCode, String) + 'static,
    {
        self.shared.borrow_mut().close_cb = Some(Box::new(f));
        self
    }

    /// Public interface for this event-loop operation.
    pub fn send(&self, message: Message) -> Result<(), crate::event_loop::Error> {
        if self.loop_.upgrade().is_none() {
            return Err(crate::event_loop::Error::LoopGone);
        }
        self.shared.borrow_mut().protocol.enqueue(message)
    }

    /// Public interface for this event-loop operation.
    pub fn close(
        &self,
        code: CloseCode,
        reason: impl Into<String>,
    ) -> Result<(), crate::event_loop::Error> {
        let reason = reason.into();
        if self.loop_.upgrade().is_none() {
            return Err(crate::event_loop::Error::LoopGone);
        }
        self.shared
            .borrow_mut()
            .protocol
            .close(code, &reason, Instant::now())?;
        self.shared.borrow_mut().state = WebSocketState::Closing;
        Ok(())
    }

    /// Returns the current protocol state.
    pub fn state(&self) -> WebSocketState {
        self.shared.borrow().state
    }

    /// Returns the underlying socket when the handshake is complete.
    pub fn raw_fd(&self) -> Option<i32> {
        self.shared.borrow().curl_socket
    }
}

impl WebSocketShared {
    /// Public interface for this event-loop operation.
    pub(crate) fn new(options: Options) -> Self {
        let protocol = connection::Connection::new(options.clone(), Instant::now());
        Self {
            protocol,
            curl_socket: None,
            state: WebSocketState::Connecting,
            message_cb: None,
            close_cb: None,
            error_cb: None,
            handler: None,
            loop_: Weak::new(),
            guard: Weak::new(),
        }
    }
}

fn dispatch(
    shared: &Rc<RefCell<WebSocketShared>>,
    callback: impl FnOnce(&mut dyn WebSocketHandler, &WebSocketClient),
) {
    let (handler, loop_, guard) = {
        let mut state = shared.borrow_mut();
        (
            state.handler.take(),
            state.loop_.clone(),
            state.guard.upgrade(),
        )
    };
    if let Some(mut handler) = handler {
        let client =
            WebSocketClient::new(shared.clone(), loop_, guard.unwrap_or_else(|| Rc::new(())));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            callback(handler.as_mut(), &client)
        }));
        shared.borrow_mut().handler = Some(handler);
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
