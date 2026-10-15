//! Single-thread socket-action event loop.
mod error;
/// Public interface for this event-loop operation.
pub use error::{AddError, Error};

use crate::easy::Easy;
use crate::multi::{EasyHandle, Multi};
use crate::reactor::{ Reactor, Ready };
use crate::websocket::connection::{Connection, FrameMeta, InboundFrame, TimerOutcome, WebSocketIo};
use crate::websocket::{
    CloseCode, Message as WsMessage, Options as WsOptions, WebSocketClient, WebSocketHandler,
    WebSocketState,
};
use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::marker::PhantomData;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
/// Public event-loop value or operation.
pub struct Token(pub(crate) usize);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
/// Token identifying a registered watcher.
pub struct WatchToken(usize);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
/// Token identifying a scheduled timer.
pub struct TimerToken(pub(crate) usize);

#[derive(Copy, Clone, Debug)]
/// Public event-loop value or operation.
pub struct WatchEvents {
    /// Public interface for this event-loop operation.
    pub readable: bool,
    /// Public interface for this event-loop operation.
    pub writable: bool,
    /// Public interface for this event-loop operation.
    pub error: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Owner {
    Curl,
    WebSocket(Token),
    User(WatchToken),
}

struct CurlWsIo(*mut curl_sys::CURL);

impl WebSocketIo for CurlWsIo {
    fn recv(&mut self, b: &mut [u8]) -> Result<Option<(usize, FrameMeta)>, Error> {
        let mut n = 0;
        let mut m: *const curl_sys::curl_ws_frame = std::ptr::null();
        let rc =
            unsafe { curl_sys::curl_ws_recv(self.0, b.as_mut_ptr() as _, b.len(), &mut n, &mut m) };
        if rc == curl_sys::CURLE_AGAIN {
            return Ok(None);
        }
        if rc != curl_sys::CURLE_OK {
            return Err(Error::Curl(crate::Error::new(rc)));
        }
        if m.is_null() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "missing websocket metadata",
            )));
        }
        Ok(Some((
            n,
            FrameMeta {
                flags: unsafe { (*m).flags },
                bytes_left: unsafe { (*m).bytesleft },
            },
        )))
    }

    fn send(&mut self, b: &[u8], flags: u32, size: i64) -> Result<Option<usize>, Error> {
        let mut n = 0;
        let rc = unsafe {
            curl_sys::curl_ws_send(self.0, b.as_ptr() as _, b.len(), &mut n, size, flags)
        };
        if rc == curl_sys::CURLE_AGAIN {
            return Ok(None);
        }
        if rc != curl_sys::CURLE_OK {
            return Err(Error::Curl(crate::Error::new(rc)));
        }
        Ok(Some(n))
    }
}

type Completion = Box<dyn FnOnce(Easy, Result<(), Error>)>;

struct QueuedRequest {
    token: Token,
    easy: Easy,
    completion: Option<Completion>,
    is_websocket: bool,
}

struct ActiveRequest {
    handle: EasyHandle,
    completion: Option<Completion>,
    is_websocket: bool,
}

pub(crate) struct WsSlot {
    pub(crate) handle: Option<EasyHandle>,
    pub(crate) fd: Option<i32>,
    pub(crate) state: WebSocketState,
    pub(crate) protocol: Connection,
    pub(crate) handler: Option<Box<dyn WebSocketHandler>>,
    pub(crate) client_count: Rc<Cell<usize>>,
    pub(crate) in_callback: bool,
    pending_write_error: Option<Error>,
}

type Watch = (i32, Option<Box<dyn FnMut(WatchEvents)>>);

struct Core {
    multi: Multi,
    requests: HashMap<Token, ActiveRequest>,
    watches: HashMap<WatchToken, Watch>,
    timers: BinaryHeap<Reverse<(Instant, usize)>>,
    timer_callbacks: HashMap<usize, Box<dyn FnOnce()>>,
}

/// Public interface for this event-loop operation.
pub(crate) struct LoopInner {
    self_weak: Weak<LoopInner>,
    core: RefCell<Core>,
    websockets: RefCell<HashMap<Token, WsSlot>>,
    queued: RefCell<VecDeque<QueuedRequest>>,
    pending_cancels: RefCell<Vec<Token>>,
    ready_buf: RefCell<Vec<Ready>>,
    ws_tokens_scratch: RefCell<Vec<Token>>,
    next_token: Cell<usize>,
    running: Cell<bool>,
    needs_timeout_bootstrap: Cell<bool>,
    next_ws_deadline: Cell<Option<Instant>>,
    reactor: Rc<RefCell<Reactor>>,
    owners: Rc<RefCell<HashMap<i32, Owner>>>,
    curl_deadline: Rc<Cell<Option<Instant>>>,
    socket_error: Rc<RefCell<Option<std::io::Error>>>,
}

/// A single-thread runner. Client handles borrow it weakly.
pub struct EventLoop {
    /// Public interface for this event-loop operation.
    pub(crate) inner: Rc<LoopInner>,
    _marker: PhantomData<*mut ()>,
}

impl std::fmt::Debug for EventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLoop").finish_non_exhaustive()
    }
}

impl LoopInner {
    fn token(&self) -> Token {
        let id = self.next_token.get();
        self.next_token
            .set(id.checked_add(1).expect("token space exhausted"));
        Token(id)
    }

    pub(crate) fn enqueue(
        &self,
        easy: Easy,
        completion: Option<Completion>,
    ) -> Result<Token, AddError> {
        if !easy.validate_targets() {
            return Err(AddError {
                easy,
                error: Error::UnresolvedHost,
            });
        }
        let token = self.token();
        self.queued.borrow_mut().push_back(QueuedRequest {
            token,
            easy,
            completion,
            is_websocket: false,
        });
        Ok(token)
    }

    pub(crate) fn enqueue_websocket(
        &self,
        easy: Easy,
        options: WsOptions,
        handler: Box<dyn WebSocketHandler>,
        client_count: Rc<Cell<usize>>,
    ) -> Result<Token, AddError> {
        if !easy.validate_targets() {
            return Err(AddError {
                easy,
                error: Error::UnresolvedHost,
            });
        }
        let token = self.token();
        let now = Instant::now();
        let protocol = Connection::new(options, now);
        self.websockets.borrow_mut().insert(
            token,
            WsSlot {
                handle: None,
                fd: None,
                state: WebSocketState::Connecting,
                protocol,
                handler: Some(handler),
                client_count,
                in_callback: false,
                pending_write_error: None,
            },
        );
        self.queued.borrow_mut().push_back(QueuedRequest {
            token,
            easy,
            completion: None,
            is_websocket: true,
        });
        Ok(token)
    }

    pub(crate) fn refresh_ws_deadline(&self) {
        let min_deadline = self
            .websockets
            .borrow()
            .values()
            .filter(|slot| matches!(slot.state, WebSocketState::Open | WebSocketState::Closing))
            .filter_map(|slot| slot.protocol.next_deadline())
            .min();
        self.next_ws_deadline.set(min_deadline);
    }

    pub(crate) fn on_ws_clients_zero(&self, token: Token) {
        let (in_callback, state) = {
            let Ok(ws_map) = self.websockets.try_borrow() else {
                return;
            };
            let Some(slot) = ws_map.get(&token) else {
                return;
            };
            (slot.in_callback, slot.state)
        };
        if in_callback {
            return;
        }
        match state {
            WebSocketState::Connecting => {
                // Cancel the queued or in-flight upgrade immediately so no ghost connection opens.
                let _ = self.cancel_connecting_ws(token);
            }
            WebSocketState::Open => {
                let _ = self.ws_close(token, CloseCode::NORMAL, "");
            }
            WebSocketState::Closing | WebSocketState::Closed => {}
        }
    }

    fn cancel_connecting_ws(&self, token: Token) {
        let removed_slot = self.websockets.borrow_mut().remove(&token);
        drop(removed_slot);
        let removed_queued = {
            let mut q = self.queued.borrow_mut();
            q.iter()
                .position(|r| r.token == token)
                .and_then(|pos| q.remove(pos))
        };
        if let Some(req) = removed_queued {
            drop(req);
            return;
        }
        if let Ok(mut core) = self.core.try_borrow_mut() {
            let removed_req = core.requests.remove(&token);
            if let Some(r) = removed_req {
                let easy = core.multi.remove(r.handle);
                drop(core);
                drop(easy);
            }
        } else {
            self.pending_cancels.borrow_mut().push(token);
        }
    }

    fn drain_pending_cancels(&self) {
        if self.pending_cancels.borrow().is_empty() {
            return;
        }
        let tokens = std::mem::take(&mut *self.pending_cancels.borrow_mut());
        for token in tokens {
            let removed = {
                let mut core = self.core.borrow_mut();
                core.requests
                    .remove(&token)
                    .map(|r| core.multi.remove(r.handle))
            };
            drop(removed);
        }
    }

    pub(crate) fn ws_state(&self, token: Token) -> WebSocketState {
        self.websockets
            .try_borrow()
            .ok()
            .and_then(|map| map.get(&token).map(|slot| slot.state))
            .unwrap_or(WebSocketState::Closed)
    }

    pub(crate) fn ws_raw_fd(&self, token: Token) -> Option<i32> {
        self.websockets
            .try_borrow()
            .ok()
            .and_then(|map| map.get(&token).and_then(|slot| slot.fd))
    }

    pub(crate) fn ws_send(&self, token: Token, message: WsMessage<'_>) -> Result<(), Error> {
        let mut ws_map = self
            .websockets
            .try_borrow_mut()
            .map_err(|_| Error::Reentrant)?;
        let Some(slot) = ws_map.get_mut(&token) else {
            return Err(Error::NotFound);
        };
        match slot.state {
            WebSocketState::Connecting => slot.protocol.enqueue(message),
            WebSocketState::Open => {
                let Some(handle) = slot.handle.as_ref() else {
                    return Err(Error::NotFound);
                };
                let curl_ptr = handle.raw();
                let fd = slot.fd;
                let res = slot.protocol.send_direct(&mut CurlWsIo(curl_ptr), message);
                let wants_write = slot.protocol.wants_write();
                drop(ws_map);
                let res = res.and_then(|()| {
                    if wants_write {
                        if let Some(fd) = fd {
                            self.reactor
                                .borrow_mut()
                                .set_interest(fd, true, true)
                                .map_err(Error::Io)?;
                        }
                    }
                    Ok(())
                });
                self.ws_write_result(token, res)
            }
            WebSocketState::Closing | WebSocketState::Closed => Err(Error::NotFound),
        }
    }

    pub(crate) fn ws_close(&self, token: Token, code: CloseCode, reason: &str) -> Result<(), Error> {
        let mut ws_map = self
            .websockets
            .try_borrow_mut()
            .map_err(|_| Error::Reentrant)?;
        let Some(slot) = ws_map.get_mut(&token) else {
            return Ok(());
        };
        match slot.state {
            WebSocketState::Closed => Ok(()),
            WebSocketState::Connecting => {
                drop(ws_map);
                self.cancel_connecting_ws(token);
                Ok(())
            }
            WebSocketState::Open | WebSocketState::Closing => {
                let now = Instant::now();
                let mut close_res = slot.protocol.close(code, reason, now);
                slot.state = WebSocketState::Closing;
                if close_res.is_ok() {
                    if let Some(handle) = slot.handle.as_ref() {
                        close_res = slot.protocol.flush(&mut CurlWsIo(handle.raw()));
                    }
                }
                let fd = slot.fd;
                let wants_write = slot.protocol.wants_write();
                drop(ws_map);
                self.refresh_ws_deadline();
                let res = close_res.and_then(|()| {
                    if let Some(fd) = fd {
                        self.reactor
                            .borrow_mut()
                            .set_interest(fd, true, wants_write)
                            .map_err(Error::Io)?;
                    }
                    Ok(())
                });
                self.ws_write_result(token, res)
            }
        }
    }

    fn ws_write_result(&self, token: Token, result: Result<(), Error>) -> Result<(), Error> {
        // The caller and the handler both receive the transport failure. Only
        // terminal transport errors close the connection, not queue rejection.
        let callback_error = match &result {
            Err(Error::Curl(error)) => Error::Curl(error.clone()),
            Err(Error::Io(error)) => Error::Io(match error.raw_os_error() {
                Some(code) => std::io::Error::from_raw_os_error(code),
                None => std::io::Error::new(error.kind(), error.to_string()),
            }),
            _ => return result,
        };
        let in_callback = {
            let mut ws_map = self.websockets.borrow_mut();
            let Some(slot) = ws_map.get_mut(&token) else {
                return result;
            };
            slot.state = WebSocketState::Closed;
            slot.pending_write_error = Some(callback_error);
            slot.in_callback
        };
        if !in_callback {
            self.finish_pending_ws_write(token);
        }
        result
    }

    fn finish_pending_ws_write(&self, token: Token) -> bool {
        let error = self
            .websockets
            .borrow_mut()
            .get_mut(&token)
            .and_then(|slot| slot.pending_write_error.take());
        if let Some(error) = error {
            self.ws_finish(
                token,
                Some((CloseCode::ABNORMAL_CLOSURE, "connection closed")),
                Some(error),
            );
            true
        } else {
            false
        }
    }

    fn dispatch_ws(
        &self,
        token: Token,
        callback: impl FnOnce(&mut dyn WebSocketHandler, &WebSocketClient),
    ) {
        let (handler, client_count) = {
            let mut ws_map = self.websockets.borrow_mut();
            let Some(slot) = ws_map.get_mut(&token) else {
                return;
            };
            slot.in_callback = true;
            (slot.handler.take(), slot.client_count.clone())
        };
        if let Some(mut handler) = handler {
            let ws_ref = WebSocketClient::callback_ref(
                self.self_weak.clone(),
                token,
                client_count.clone(),
            );
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                callback(handler.as_mut(), &ws_ref)
            }));
            let (state_after, dropped_handler) = {
                let mut ws_map = self.websockets.borrow_mut();
                if let Some(slot) = ws_map.get_mut(&token) {
                    slot.handler = Some(handler);
                    slot.in_callback = false;
                    (Some(slot.state), None)
                } else {
                    (None, Some(handler))
                }
            };
            drop(dropped_handler);
            if let Err(panic) = result {
                // Restore and clean up the connection even if the callback
                // panics after encountering a write error; preserve its panic.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.finish_pending_ws_write(token)
                }));
                std::panic::resume_unwind(panic);
            }
            if self.finish_pending_ws_write(token) {
                return;
            }
            if client_count.get() == 0 && state_after == Some(WebSocketState::Open) {
                let _ = self.ws_close(token, CloseCode::NORMAL, "");
            }
        } else {
            let mut ws_map = self.websockets.borrow_mut();
            if let Some(slot) = ws_map.get_mut(&token) {
                slot.in_callback = false;
            }
        }
    }

    fn ws_finish(
        &self,
        token: Token,
        close_info: Option<(CloseCode, &str)>,
        error: Option<Error>,
    ) {
        let (handle, fd) = {
            let mut ws_map = self.websockets.borrow_mut();
            let Some(slot) = ws_map.get_mut(&token) else {
                return;
            };
            slot.state = WebSocketState::Closed;
            (slot.handle.take(), slot.fd.take())
        };
        if let Some(fd) = fd {
            let _ = self.reactor.borrow_mut().deregister(fd);
            self.owners.borrow_mut().remove(&fd);
        }
        if let Some(handle) = handle {
            let removed_easy = self.core.borrow().multi.remove(handle);
            drop(removed_easy);
        }
        self.refresh_ws_deadline();

        if let Some(err) = error {
            self.dispatch_ws(token, |handler, ws| handler.on_error(ws, err));
        }
        if let Some((code, reason)) = close_info {
            self.dispatch_ws(token, |handler, ws| handler.on_close(ws, code, reason));
        }
        let removed_slot = self.websockets.borrow_mut().remove(&token);
        drop(removed_slot);
    }

    fn ws_on_readable(&self, token: Token) {
        let (mut handler, mut buffers, curl_ptr, client_count) = {
            let mut ws_map = self.websockets.borrow_mut();
            let Some(slot) = ws_map.get_mut(&token) else {
                return;
            };
            let Some(handle) = slot.handle.as_ref() else {
                return;
            };
            slot.in_callback = true;
            (
                slot.handler.take(),
                slot.protocol.take_buffers(),
                handle.raw(),
                slot.client_count.clone(),
            )
        };

        let ws_ref = WebSocketClient::callback_ref(
            self.self_weak.clone(),
            token,
            client_count.clone(),
        );

        enum ReadExit {
            Continue,
            PeerClose(CloseCode, String),
            ProtocolReject(CloseCode, Error),
            TransportClose,
            TransportError(Error),
        }

        let mut exit = ReadExit::Continue;

        loop {
            let now = Instant::now();
            let step = {
                let mut ws_map = self.websockets.borrow_mut();
                let Some(slot) = ws_map.get_mut(&token) else {
                    break;
                };
                slot.protocol
                    .receive_step(&mut buffers, &mut CurlWsIo(curl_ptr), now)
            };

            match step {
                Ok(None) => break,
                Ok(Some(InboundFrame::Message(msg))) => {
                    if let Some(h) = handler.as_mut() {
                        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            h.on_message(&ws_ref, msg)
                        }));
                        if let Err(panic) = res {
                            let mut ws_map = self.websockets.borrow_mut();
                            if let Some(slot) = ws_map.get_mut(&token) {
                                slot.protocol.put_buffers(buffers);
                                slot.handler = handler;
                                slot.in_callback = false;
                            }
                            drop(ws_map);
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                self.finish_pending_ws_write(token)
                            }));
                            std::panic::resume_unwind(panic);
                        }
                    }
                    let (still_active, is_open) = self
                        .websockets
                        .borrow()
                        .get(&token)
                        .map_or((false, false), |s| {
                            (
                                matches!(s.state, WebSocketState::Open | WebSocketState::Closing),
                                s.state == WebSocketState::Open,
                            )
                        });
                    if !still_active || (client_count.get() == 0 && is_open) {
                        break;
                    }
                }
                Ok(Some(InboundFrame::PeerClose { code, reason })) => {
                    exit = ReadExit::PeerClose(code, reason.to_owned());
                    break;
                }
                Err(Error::MessageTooLarge) => {
                    exit = ReadExit::ProtocolReject(CloseCode::MESSAGE_TOO_BIG, Error::MessageTooLarge);
                    break;
                }
                Err(Error::InvalidUtf8) => {
                    exit = ReadExit::ProtocolReject(CloseCode::INVALID_PAYLOAD, Error::InvalidUtf8);
                    break;
                }
                Err(Error::Curl(err)) if err.code() == curl_sys::CURLE_GOT_NOTHING => {
                    exit = ReadExit::TransportClose;
                    break;
                }
                Err(Error::Curl(err)) if err.code() == curl_sys::CURLE_FTP_WEIRD_SERVER_REPLY => {
                    exit = ReadExit::ProtocolReject(CloseCode::PROTOCOL_ERROR, Error::Curl(err));
                    break;
                }
                Err(error) => {
                    exit = ReadExit::TransportError(error);
                    break;
                }
            }
        }

        let (post_state, dropped_handler) = {
            let mut ws_map = self.websockets.borrow_mut();
            if let Some(slot) = ws_map.get_mut(&token) {
                slot.protocol.put_buffers(buffers);
                slot.handler = handler;
                slot.in_callback = false;
                (Some((slot.fd, slot.protocol.wants_write(), slot.state)), None)
            } else {
                (None, handler)
            }
        };
        drop(dropped_handler);
        if self.finish_pending_ws_write(token) {
            return;
        }

        match exit {
            ReadExit::Continue => {
                if let Some((Some(fd), wants_write, state)) = post_state {
                    if let Err(err) = self.reactor.borrow_mut().set_interest(fd, true, wants_write)
                    {
                        self.ws_finish(
                            token,
                            Some((CloseCode::ABNORMAL_CLOSURE, "connection closed")),
                            Some(Error::Io(err)),
                        );
                        return;
                    }
                    self.refresh_ws_deadline();
                    if client_count.get() == 0 && state == WebSocketState::Open {
                        let _ = self.ws_close(token, CloseCode::NORMAL, "");
                    }
                }
            }
            ReadExit::PeerClose(code, reason) => {
                self.ws_finish(token, Some((code, &reason)), None);
            }
            ReadExit::ProtocolReject(code, err) => {
                self.ws_finish(token, Some((code, "websocket message rejected")), Some(err));
            }
            ReadExit::TransportClose => {
                self.ws_finish(
                    token,
                    Some((CloseCode::ABNORMAL_CLOSURE, "connection closed")),
                    None,
                );
            }
            ReadExit::TransportError(err) => {
                self.ws_finish(
                    token,
                    Some((CloseCode::ABNORMAL_CLOSURE, "connection closed")),
                    Some(err),
                );
            }
        }
    }

    fn ws_on_writable(&self, token: Token) {
        let outcome = {
            let mut ws_map = self.websockets.borrow_mut();
            let Some(slot) = ws_map.get_mut(&token) else {
                return;
            };
            let Some(handle) = slot.handle.as_ref() else {
                return;
            };
            let curl_ptr = handle.raw();
            let res = slot.protocol.flush(&mut CurlWsIo(curl_ptr));
            (res, slot.fd, slot.protocol.wants_write())
        };
        match outcome {
            (Ok(()), Some(fd), wants_write) => {
                if let Err(err) = self.reactor.borrow_mut().set_interest(fd, true, wants_write) {
                    self.ws_finish(
                        token,
                        Some((CloseCode::ABNORMAL_CLOSURE, "connection closed")),
                        Some(Error::Io(err)),
                    );
                }
            }
            (Ok(()), None, _) => {}
            (Err(err), _, _) => {
                self.ws_finish(
                    token,
                    Some((CloseCode::ABNORMAL_CLOSURE, "connection closed")),
                    Some(err),
                );
            }
        }
    }

    fn ws_on_timer(&self, token: Token, now: Instant) {
        let outcome = {
            let mut ws_map = self.websockets.borrow_mut();
            let Some(slot) = ws_map.get_mut(&token) else {
                return;
            };
            if !matches!(slot.state, WebSocketState::Open | WebSocketState::Closing) {
                return;
            }
            let mut res = slot.protocol.on_timer(now);
            if res.is_ok() && slot.protocol.wants_write() {
                if let Some(handle) = slot.handle.as_ref() {
                    if let Err(flush_err) = slot.protocol.flush(&mut CurlWsIo(handle.raw())) {
                        res = Err(flush_err);
                    }
                }
            }
            (res, slot.fd, slot.protocol.wants_write())
        };

        match outcome {
            (Ok(TimerOutcome::Continue), Some(fd), wants_write) => {
                if let Err(err) = self.reactor.borrow_mut().set_interest(fd, true, wants_write) {
                    self.ws_finish(
                        token,
                        Some((CloseCode::ABNORMAL_CLOSURE, "connection closed")),
                        Some(Error::Io(err)),
                    );
                }
            }
            (Ok(TimerOutcome::Continue), None, _) => {}
            (Ok(TimerOutcome::Close(code, reason)), _, _) => {
                self.ws_finish(token, Some((code, reason)), None);
            }
            (Err(Error::CloseTimeout), _, _) => {
                self.ws_finish(
                    token,
                    Some((CloseCode::GOING_AWAY, "close timeout")),
                    None,
                );
            }
            (Err(err), _, _) => {
                self.ws_finish(
                    token,
                    Some((CloseCode::ABNORMAL_CLOSURE, "connection closed")),
                    Some(err),
                );
            }
        }
    }

    /// Public interface for this event-loop operation.
    pub(crate) fn cancel(&self, token: Token) -> Result<Easy, Error> {
        let queued = {
            let mut q = self.queued.borrow_mut();
            q.iter()
                .position(|r| r.token == token)
                .and_then(|i| q.remove(i))
        };
        if let Some(r) = queued {
            let removed_slot = self.websockets.borrow_mut().remove(&token);
            drop(removed_slot);
            return Ok(r.easy);
        }
        let removed_req = {
            let mut core = self.core.try_borrow_mut().map_err(|_| Error::Reentrant)?;
            core.requests.remove(&token)
        };
        if let Some(r) = removed_req {
            let removed_slot = self.websockets.borrow_mut().remove(&token);
            drop(removed_slot);
            let res = self
                .core
                .try_borrow_mut()
                .map_err(|_| Error::Reentrant)?
                .multi
                .remove(r.handle)
                .map_err(Error::Multi);
            drop(r.completion);
            return res;
        }
        let mut slot = {
            let mut ws_map = self
                .websockets
                .try_borrow_mut()
                .map_err(|_| Error::Reentrant)?;
            ws_map.remove(&token)
        };
        if let Some(ref mut slot) = slot {
            slot.state = WebSocketState::Closed;
            if let Some(fd) = slot.fd.take() {
                self.reactor
                    .borrow_mut()
                    .deregister(fd)
                    .map_err(Error::Io)?;
                self.owners.borrow_mut().remove(&fd);
            }
            let handle = slot.handle.take().ok_or(Error::NotFound)?;
            let result = self
                .core
                .try_borrow_mut()
                .map_err(|_| Error::Reentrant)?
                .multi
                .remove(handle)
                .map_err(Error::Multi);
            self.refresh_ws_deadline();
            if let Some(mut handler) = slot.handler.take() {
                let ws_ref = WebSocketClient::callback_ref(
                    self.self_weak.clone(),
                    token,
                    slot.client_count.clone(),
                );
                handler.on_close(&ws_ref, CloseCode::ABNORMAL_CLOSURE, "connection closed");
            }
            return result;
        }
        Err(Error::NotFound)
    }

    /// Public interface for this event-loop operation.
    pub(crate) fn with<F, R>(&self, token: Token, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Easy) -> R,
    {
        let queued = {
            let mut q = self
                .queued
                .try_borrow_mut()
                .map_err(|_| Error::Reentrant)?;
            q.iter()
                .position(|r| r.token == token)
                .and_then(|i| q.remove(i))
        };
        if let Some(mut r) = queued {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut r.easy)));
            self.queued.borrow_mut().push_back(r);
            return match result {
                Ok(v) => Ok(v),
                Err(p) => std::panic::resume_unwind(p),
            };
        }
        let mut r = self
            .core
            .try_borrow_mut()
            .map_err(|_| Error::Reentrant)?
            .requests
            .remove(&token)
            .ok_or(Error::NotFound)?;
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(r.handle.easy_mut())));
        self.core.borrow_mut().requests.insert(token, r);
        match result {
            Ok(v) => Ok(v),
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    fn apply_queued(&self) {
        let mut q = self.queued.borrow_mut();
        if q.is_empty() {
            return;
        }
        // A completion can enqueue a follow-up without extending this batch indefinitely.
        let batch = std::mem::take(&mut *q);
        drop(q);
        for request in batch {
            let result = self.core.borrow().multi.add(request.easy);
            match result {
                Ok(mut handle) => {
                    let _ = handle.set_token(request.token.0);
                    self.needs_timeout_bootstrap.set(true);
                    self.core.borrow_mut().requests.insert(
                        request.token,
                        ActiveRequest {
                            handle,
                            completion: request.completion,
                            is_websocket: request.is_websocket,
                        },
                    );
                }
                Err((easy, error)) => {
                    if request.is_websocket {
                        self.ws_finish(
                            request.token,
                            Some((CloseCode::ABNORMAL_CLOSURE, "connection failed")),
                            Some(Error::Multi(error)),
                        );
                    } else if let Some(cb) = request.completion {
                        cb(easy, Err(Error::Multi(error)));
                    }
                }
            }
        }
    }

    fn socket_check(&self) -> Result<(), Error> {
        if let Some(error) = self.socket_error.borrow_mut().take() {
            return Err(Error::Io(error));
        }
        Ok(())
    }

    fn action(
        &self,
        fd: curl_sys::curl_socket_t,
        events: &crate::multi::Events,
    ) -> Result<(), Error> {
        let res = self.core.borrow().multi.action(fd, events);
        self.drain_pending_cancels();
        res?;
        crate::panic::propagate();
        self.socket_check()
    }

    fn complete_requests(&self) -> Result<(), Error> {
        let mut done = Vec::new();
        {
            let core = self.core.borrow();
            core.multi.messages(|msg| {
                if let Ok(raw_token) = msg.token() {
                    let token = Token(raw_token);
                    if let Some(r) = core.requests.get(&token) {
                        if let Some(result) = msg.result_for(&r.handle) {
                            done.push((token, result));
                        }
                    }
                }
            });
        }
        crate::panic::propagate();
        for (token, result) in done {
            let Some(request) = self.core.borrow_mut().requests.remove(&token) else {
                continue;
            };
            if request.is_websocket {
                self.complete_ws_handshake(token, request.handle, result)?;
            } else {
                let easy = self.core.borrow().multi.remove(request.handle)?;
                if let Some(cb) = request.completion {
                    cb(easy, result.map_err(Error::Curl));
                }
            }
        }
        Ok(())
    }

    fn complete_ws_handshake(
        &self,
        token: Token,
        mut handle: EasyHandle,
        result: Result<(), crate::Error>,
    ) -> Result<(), Error> {
        if !self.websockets.borrow().contains_key(&token) {
            let removed = self.core.borrow().multi.remove(handle);
            drop(removed);
            return Ok(());
        }
        if let Err(error) = result {
            let removed = self.core.borrow().multi.remove(handle);
            drop(removed);
            self.ws_finish(
                token,
                Some((CloseCode::ABNORMAL_CLOSURE, "connection failed")),
                Some(Error::Curl(error)),
            );
            return Ok(());
        }
        let Some(fd) = handle.easy_mut().active_socket() else {
            let removed = self.core.borrow().multi.remove(handle);
            drop(removed);
            self.ws_finish(
                token,
                Some((CloseCode::ABNORMAL_CLOSURE, "connection failed")),
                Some(Error::HandshakeFailed),
            );
            return Ok(());
        };
        if let Err(error) = self.reactor.borrow_mut().set_interest(fd, true, false) {
            let removed = self.core.borrow().multi.remove(handle);
            drop(removed);
            self.ws_finish(
                token,
                Some((CloseCode::ABNORMAL_CLOSURE, "connection failed")),
                Some(Error::Io(error)),
            );
            return Ok(());
        }
        self.owners.borrow_mut().insert(fd, Owner::WebSocket(token));
        {
            let mut ws_map = self.websockets.borrow_mut();
            if let Some(slot) = ws_map.get_mut(&token) {
                slot.protocol.on_open(Instant::now());
                slot.handle = Some(handle);
                slot.fd = Some(fd);
                slot.state = WebSocketState::Open;
            }
        }
        self.dispatch_ws(token, |handler, ws| handler.on_open(ws));
        if self.websockets.borrow().contains_key(&token) {
            self.ws_on_writable(token);
            // The upgrade response can contain the first WebSocket frames.
            // Drain libcurl's read-ahead without waiting for another fd event.
            self.ws_on_readable(token);
            self.refresh_ws_deadline();
        }
        Ok(())
    }

    fn active(&self) -> bool {
        let core = self.core.borrow();
        !self.queued.borrow().is_empty()
            || !core.requests.is_empty()
            || !self.websockets.borrow().is_empty()
            || !core.timer_callbacks.is_empty()
            || !core.watches.is_empty()
    }

    fn next_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        if !self.queued.borrow().is_empty() || self.needs_timeout_bootstrap.get() {
            return Some(Duration::ZERO);
        }
        let mut core = self.core.borrow_mut();
        let curl = if core.requests.is_empty() {
            None
        } else {
            self.curl_deadline.get()
        };
        let user = loop {
            match core.timers.peek().copied() {
                Some(Reverse((at, id))) => {
                    if core.timer_callbacks.contains_key(&id) {
                        break Some(at);
                    }
                    core.timers.pop();
                }
                None => break None,
            }
        };
        let ws = self.next_ws_deadline.get();
        [curl, user, ws]
            .into_iter()
            .flatten()
            .min()
            .map(|at| at.saturating_duration_since(now))
    }
}

impl Drop for LoopInner {
    fn drop(&mut self) {
        let core = self.core.get_mut();
        let ws_map = self.websockets.get_mut();
        for (_, mut slot) in ws_map.drain() {
            slot.state = WebSocketState::Closed;
            if let Some(fd) = slot.fd.take() {
                let _ = self.reactor.borrow_mut().deregister(fd);
                self.owners.borrow_mut().remove(&fd);
            }
            if let Some(handle) = slot.handle.take() {
                let _ = core.multi.remove(handle);
            }
        }
        for (_, r) in core.requests.drain() {
            let _ = core.multi.remove(r.handle);
        }
    }
}

impl EventLoop {
    /// Public interface for this event-loop operation.
    pub fn new() -> Result<Self, Error> {
        let reactor = Rc::new(RefCell::new(Reactor::new().map_err(Error::Io)?));
        let owners = Rc::new(RefCell::new(HashMap::<i32, Owner>::new()));
        let curl_deadline = Rc::new(Cell::new(None));
        let socket_error = Rc::new(RefCell::new(None));
        let mut multi = Multi::new();
        let (r, o, e) = (reactor.clone(), owners.clone(), socket_error.clone());
        multi.socket_function(move |socket, events, _| {
            let fd = socket as i32;
            let owner = o.borrow().get(&fd).copied();
            if matches!(owner, Some(Owner::WebSocket(_) | Owner::User(_))) {
                return;
            }
            let result = if events.remove() {
                o.borrow_mut().remove(&fd);
                r.borrow_mut().deregister(fd)
            } else {
                o.borrow_mut().insert(fd, Owner::Curl);
                r.borrow_mut().set_interest(
                    fd,
                    events.input() || events.input_and_output(),
                    events.output() || events.input_and_output(),
                )
            };
            if let Err(error) = result {
                *e.borrow_mut() = Some(error);
            }
        })?;
        let deadline = curl_deadline.clone();
        multi.timer_function(move |d| {
            deadline.set(d.map(|d| Instant::now() + d));
            true
        })?;
        let inner = Rc::new_cyclic(|weak| LoopInner {
            self_weak: weak.clone(),
            core: RefCell::new(Core {
                multi,
                requests: HashMap::new(),
                watches: HashMap::new(),
                timers: BinaryHeap::new(),
                timer_callbacks: HashMap::new(),
            }),
            websockets: RefCell::new(HashMap::new()),
            queued: RefCell::new(VecDeque::new()),
            pending_cancels: RefCell::new(Vec::new()),
            ready_buf: RefCell::new(Vec::with_capacity(64)),
            ws_tokens_scratch: RefCell::new(Vec::new()),
            next_token: Cell::new(0),
            running: Cell::new(false),
            needs_timeout_bootstrap: Cell::new(false),
            next_ws_deadline: Cell::new(None),
            reactor,
            owners,
            curl_deadline,
            socket_error,
        });
        Ok(Self {
            inner,
            _marker: PhantomData,
        })
    }

    /// Sets the maximum number of simultaneously open connections in total (`CURLMOPT_MAX_TOTAL_CONNECTIONS`).
    pub fn max_total_connections(&self, max: usize) -> Result<(), Error> {
        self.inner
            .core
            .try_borrow_mut()
            .map_err(|_| Error::Reentrant)?
            .multi
            .max_total_connections(max)
            .map_err(Error::Multi)
    }

    /// Sets the maximum number of simultaneously open connections to a single host (`CURLMOPT_MAX_HOST_CONNECTIONS`).
    pub fn max_host_connections(&self, max: usize) -> Result<(), Error> {
        self.inner
            .core
            .try_borrow_mut()
            .map_err(|_| Error::Reentrant)?
            .multi
            .max_host_connections(max)
            .map_err(Error::Multi)
    }

    /// Sets the maximum number of connections kept in the connection cache (`CURLMOPT_MAXCONNECTS`).
    pub fn max_connects(&self, max: usize) -> Result<(), Error> {
        self.inner
            .core
            .try_borrow_mut()
            .map_err(|_| Error::Reentrant)?
            .multi
            .max_connects(max)
            .map_err(Error::Multi)
    }

    /// Enables or disables HTTP/1.1 pipelining and HTTP/2 multiplexing (`CURLMOPT_PIPELINING`).
    pub fn pipelining(&self, http1: bool, multiplex: bool) -> Result<(), Error> {
        self.inner
            .core
            .try_borrow_mut()
            .map_err(|_| Error::Reentrant)?
            .multi
            .pipelining(http1, multiplex)
            .map_err(Error::Multi)
    }

    /// Public interface for this event-loop operation.
    pub fn add<F>(&self, easy: Easy, on_done: F) -> Result<Token, AddError>
    where
        F: FnOnce(Easy, Result<(), Error>) + 'static,
    {
        self.inner.enqueue(easy, Some(Box::new(on_done)))
    }

    /// Public interface for this event-loop operation.
    pub fn cancel(&self, token: Token) -> Result<Easy, Error> {
        self.inner.cancel(token)
    }

    /// Public interface for this event-loop operation.
    pub fn with<F, R>(&self, token: Token, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Easy) -> R,
    {
        self.inner.with(token, f)
    }

    /// Public interface for this event-loop operation.
    pub fn watch<F>(&self, fd: i32, events: WatchEvents, callback: F) -> Result<WatchToken, Error>
    where
        F: FnMut(WatchEvents) + 'static,
    {
        if self.inner.owners.borrow().contains_key(&fd) {
            return Err(Error::AlreadyRegistered);
        }
        let mut core = self
            .inner
            .core
            .try_borrow_mut()
            .map_err(|_| Error::Reentrant)?;
        let token = WatchToken(self.inner.token().0);
        self.inner
            .reactor
            .borrow_mut()
            .set_interest(fd, events.readable, events.writable)
            .map_err(Error::Io)?;
        self.inner
            .owners
            .borrow_mut()
            .insert(fd, Owner::User(token));
        core.watches.insert(token, (fd, Some(Box::new(callback))));
        Ok(token)
    }

    /// Public interface for this event-loop operation.
    pub fn unwatch(&self, token: WatchToken) -> Result<(), Error> {
        let (fd, callback) = {
            let mut core = self
                .inner
                .core
                .try_borrow_mut()
                .map_err(|_| Error::Reentrant)?;
            core.watches.remove(&token).ok_or(Error::NotFound)?
        };
        self.inner.owners.borrow_mut().remove(&fd);
        let res = self
            .inner
            .reactor
            .borrow_mut()
            .deregister(fd)
            .map_err(Error::Io);
        drop(callback);
        res
    }

    /// Schedules a one-shot timer at `at`, returning a [`TimerToken`] that can be passed to [`EventLoop::cancel_timer`].
    pub fn add_timer<F>(&self, at: Instant, callback: F) -> TimerToken
    where
        F: FnOnce() + 'static,
    {
        let id = self.inner.token().0;
        let mut core = self.inner.core.borrow_mut();
        core.timer_callbacks.insert(id, Box::new(callback));
        core.timers.push(Reverse((at, id)));
        TimerToken(id)
    }

    /// Schedules a one-shot timer to fire after `delay`, returning a [`TimerToken`] that can be passed to [`EventLoop::cancel_timer`].
    pub fn schedule_timeout<F>(&self, delay: Duration, callback: F) -> TimerToken
    where
        F: FnOnce() + 'static,
    {
        self.add_timer(Instant::now() + delay, callback)
    }

    /// Cancels a pending timer in $O(1)$ time, returning `true` if the timer was still pending.
    pub fn cancel_timer(&self, token: TimerToken) -> bool {
        let removed = {
            let mut core = self.inner.core.borrow_mut();
            let removed = core.timer_callbacks.remove(&token.0);
            if core.timer_callbacks.is_empty() {
                core.timers.clear();
            }
            removed
        };
        let was_some = removed.is_some();
        drop(removed);
        was_some
    }

    /// Public interface for this event-loop operation.
    pub fn run_until<F>(&mut self, mut stop: F) -> Result<(), Error>
    where
        F: FnMut(&Self) -> bool,
    {
        while !stop(self) {
            if !self.inner.active() {
                break;
            }
            let wait = self.next_timeout().unwrap_or(Duration::from_secs(3600));
            if !self.run_once(wait)? {
                break;
            }
        }
        Ok(())
    }

    /// Public interface for this event-loop operation.
    pub fn run(&mut self) -> Result<(), Error> {
        while self.inner.active() {
            let wait = self.next_timeout().unwrap_or(Duration::from_secs(3600));
            self.run_once(wait)?;
        }
        Ok(())
    }

    /// Public interface for this event-loop operation.
    pub fn next_timeout(&self) -> Option<Duration> {
        self.inner.next_timeout()
    }

    /// Public interface for this event-loop operation.
    pub fn as_raw_fd(&self) -> i32 {
        self.inner.reactor.borrow().as_raw_fd()
    }

    /// Public interface for this event-loop operation.
    pub fn run_once(&mut self, wait: Duration) -> Result<bool, Error> {
        if self.inner.running.replace(true) {
            return Err(Error::Reentrant);
        }
        struct Running<'a>(&'a Cell<bool>);
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _running = Running(&self.inner.running);

        self.inner.apply_queued();

        let should_fire_curl_timeout = self.inner.needs_timeout_bootstrap.replace(false)
            || self
                .inner
                .curl_deadline
                .get()
                .is_some_and(|at| at <= Instant::now());
        if should_fire_curl_timeout {
            self.inner.curl_deadline.set(None);
            self.inner
                .action(curl_sys::CURL_SOCKET_TIMEOUT, &crate::multi::Events::new())?;
            self.inner.complete_requests()?;
        }

        // Bootstrap callbacks may have completed or cancelled the last task.
        if !self.inner.active() {
            return Ok(false);
        }
        let timeout = self.next_timeout().unwrap_or(wait).min(wait);
        let mut ready = std::mem::take(&mut *self.inner.ready_buf.borrow_mut());
        let wait_result = self
            .inner
            .reactor
            .borrow()
            .wait_into(timeout, &mut ready)
            .map_err(Error::Io);
        if let Err(err) = wait_result {
            *self.inner.ready_buf.borrow_mut() = ready;
            return Err(err);
        }

        for event in &ready {
            let fd = event.key as i32;
            let owner = self.inner.owners.borrow().get(&fd).copied();
            match owner {
                Some(Owner::WebSocket(token)) => {
                    if event.readable || event.error {
                        self.inner.ws_on_readable(token);
                    }
                    if event.writable {
                        self.inner.ws_on_writable(token);
                    }
                }
                Some(Owner::User(token)) => {
                    let callback = self
                        .inner
                        .core
                        .borrow_mut()
                        .watches
                        .get_mut(&token)
                        .and_then(|(_, cb)| cb.take());
                    if let Some(mut callback) = callback {
                        struct WatchGuard<'a> {
                            inner: &'a LoopInner,
                            token: WatchToken,
                            callback: Option<Box<dyn FnMut(WatchEvents)>>,
                        }
                        impl Drop for WatchGuard<'_> {
                            fn drop(&mut self) {
                                let dropped_cb = {
                                    let mut core = self.inner.core.borrow_mut();
                                    if let Some(cb) = self.callback.take() {
                                        if let Some(entry) = core.watches.get_mut(&self.token) {
                                            entry.1 = Some(cb);
                                            None
                                        } else {
                                            Some(cb)
                                        }
                                    } else {
                                        if let Some((fd, _)) = core.watches.remove(&self.token) {
                                            self.inner.owners.borrow_mut().remove(&fd);
                                            let _ = self.inner.reactor.borrow_mut().deregister(fd);
                                        }
                                        None
                                    }
                                };
                                drop(dropped_cb);
                            }
                        }
                        let mut guard = WatchGuard {
                            inner: &self.inner,
                            token,
                            callback: None,
                        };
                        callback(WatchEvents {
                            readable: event.readable,
                            writable: event.writable,
                            error: event.error,
                        });
                        guard.callback = Some(callback);
                    }
                }
                Some(Owner::Curl) => {
                    let mut events = crate::multi::Events::new();
                    events.input(event.readable);
                    events.output(event.writable);
                    events.error(event.error);
                    if let Err(err) = self.inner.action(fd as _, &events) {
                        ready.clear();
                        *self.inner.ready_buf.borrow_mut() = ready;
                        return Err(err);
                    }
                }
                None => {}
            }
        }
        ready.clear();
        *self.inner.ready_buf.borrow_mut() = ready;

        if self
            .inner
            .curl_deadline
            .get()
            .is_some_and(|at| at <= Instant::now())
        {
            self.inner.curl_deadline.set(None);
            self.inner
                .action(curl_sys::CURL_SOCKET_TIMEOUT, &crate::multi::Events::new())?;
        }
        self.inner.complete_requests()?;

        let now = Instant::now();
        if self
            .inner
            .next_ws_deadline
            .get()
            .is_some_and(|deadline| now >= deadline)
        {
            let mut tokens = std::mem::take(&mut *self.inner.ws_tokens_scratch.borrow_mut());
            tokens.clear();
            tokens.extend(self.inner.websockets.borrow().keys().copied());
            for &token in &tokens {
                self.inner.ws_on_timer(token, now);
            }
            tokens.clear();
            *self.inner.ws_tokens_scratch.borrow_mut() = tokens;
            self.inner.refresh_ws_deadline();
        }

        loop {
            let callback = {
                let mut core = self.inner.core.borrow_mut();
                loop {
                    match core.timers.peek().copied() {
                        Some(Reverse((at, id))) if at <= now => {
                            core.timers.pop();
                            if let Some(cb) = core.timer_callbacks.remove(&id) {
                                break Some(cb);
                            }
                        }
                        _ => break None,
                    }
                }
            };
            match callback {
                Some(cb) => cb(),
                None => break,
            }
        }

        self.inner.apply_queued();
        Ok(self.inner.active())
    }
}

