use super::{CloseCode, Message, WebSocketShared, WebSocketState};
use crate::event_loop::Error;
use crate::event_loop::driver::{Driver, DriverAction, DriverCx};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

struct Io(*mut curl_sys::CURL);

impl super::connection::WebSocketIo for Io {
    fn recv(
        &mut self,
        b: &mut [u8],
    ) -> Result<Option<(usize, super::connection::FrameMeta)>, Error> {
        let mut n = 0;
        let mut m: *const curl_sys::curl_ws_frame = std::ptr::null();
        let rc =
            unsafe { curl_sys::curl_ws_recv(self.0, b.as_mut_ptr() as _, b.len(), &mut n, &mut m) };
        if rc == curl_sys::CURLE_AGAIN {
            return Ok(None);
        };
        if rc != curl_sys::CURLE_OK {
            return Err(Error::Curl(crate::Error::new(rc)));
        };
        if m.is_null() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "missing websocket metadata",
            )));
        };
        Ok(Some((
            n,
            super::connection::FrameMeta {
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
        };
        if rc != curl_sys::CURLE_OK {
            return Err(Error::Curl(crate::Error::new(rc)));
        };
        Ok(Some(n))
    }
}
pub(crate) struct RuntimeDriver {
    pub(crate) shared: Rc<RefCell<WebSocketShared>>,
    pub(crate) ending: Option<(CloseCode, String)>,
}

impl Driver for RuntimeDriver {
    fn closed(&mut self) {
        let callback = {
            let mut shared = self.shared.borrow_mut();
            shared.state = WebSocketState::Closed;
            shared.curl_socket = None;
            shared.close_cb.take()
        };
        let (code, reason) = self
            .ending
            .take()
            .unwrap_or((CloseCode(1006), "connection closed".into()));
        super::dispatch(&self.shared, |handler, ws| {
            handler.on_close(ws, code, reason.clone())
        });
        if let Some(mut cb) = callback {
            cb(code, reason);
        }
    }

    fn on_readable(&mut self, cx: &mut DriverCx) -> Result<DriverAction, Error> {
        let result = self
            .shared
            .borrow_mut()
            .protocol
            .receive(&mut Io(cx.curl), Instant::now());
        let msgs = match result {
            Ok(msgs) => msgs,
            Err(Error::Curl(_)) | Err(Error::Io(_)) => {
                self.ending = Some((CloseCode(1006), "connection closed".into()));
                return Ok(DriverAction::Close);
            }
            Err(error @ (Error::MessageTooLarge | Error::InvalidUtf8)) => {
                let code = if matches!(error, Error::MessageTooLarge) {
                    CloseCode(1009)
                } else {
                    CloseCode(1007)
                };
                self.ending = Some((code, "websocket message rejected".into()));
                super::dispatch(&self.shared, |handler, ws| handler.on_error(ws, error));
                return Ok(DriverAction::Close);
            }
            Err(error) => return Err(error),
        };
        for m in msgs {
            if let Message::Close { code, reason } = &m {
                self.ending = Some((*code, reason.clone()));
                let now = Instant::now();
                let mut shared = self.shared.borrow_mut();
                if shared.protocol.close(*code, reason, now).is_ok() {
                    let _ = shared.protocol.flush(&mut Io(cx.curl));
                }
            }
            super::dispatch(&self.shared, |handler, ws| {
                handler.on_message(ws, m.clone())
            });
            let callback = self.shared.borrow_mut().message_cb.take();
            if let Some(mut cb) = callback {
                cb(m);
                if self.shared.borrow().message_cb.is_none() {
                    self.shared.borrow_mut().message_cb = Some(cb);
                }
            }
            if self.ending.is_some() {
                return Ok(DriverAction::Close);
            }
        }
        Ok(DriverAction::Continue)
    }

    fn on_writable(&mut self, cx: &mut DriverCx) -> Result<DriverAction, Error> {
        self.shared.borrow_mut().protocol.flush(&mut Io(cx.curl))?;
        Ok(DriverAction::Continue)
    }

    fn on_timer(&mut self, now: Instant, _: &mut DriverCx) -> Result<DriverAction, Error> {
        match self.shared.borrow_mut().protocol.on_timer(now) {
            Ok(super::connection::TimerOutcome::Continue) => Ok(DriverAction::Continue),
            Ok(super::connection::TimerOutcome::Close(code, reason)) => {
                self.ending = Some((code, reason));
                Ok(DriverAction::Close)
            }
            Err(Error::CloseTimeout) => {
                self.ending = Some((CloseCode::GOING_AWAY, "close timeout".into()));
                Ok(DriverAction::Close)
            }
            Err(error) => Err(error),
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.shared.borrow().protocol.next_deadline()
    }

    fn wants_write(&self) -> bool {
        self.shared.borrow().protocol.wants_write()
    }
}
