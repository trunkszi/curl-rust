mod support;

use curl::EventLoop;
use curl::easy::Easy;
use curl::event_loop::Error;
use curl::websocket::{
    CloseCode, Message, Options, WebSocketClient, WebSocketHandler, WebSocketState,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
enum FailAt {
    External,
    Open,
    Message,
}

fn fail_write(ws: &WebSocketClient, closing: bool) {
    assert_eq!(
        unsafe { libc::shutdown(ws.raw_fd().unwrap(), libc::SHUT_WR) },
        0
    );
    let result = if closing {
        ws.close(CloseCode::NORMAL, "")
    } else {
        ws.send_text("failed write")
    };
    assert!(
        matches!(result, Err(Error::Curl(ref error)) if error.code() == curl_sys::CURLE_SEND_ERROR)
    );
    assert_eq!(ws.state(), WebSocketState::Closed);
}

fn assert_write_failure_is_reported(at: FailAt, closing: bool) {
    struct Handler {
        at: FailAt,
        closing: bool,
        events: Rc<RefCell<Vec<&'static str>>>,
    }
    impl WebSocketHandler for Handler {
        fn on_open(&mut self, ws: &WebSocketClient) {
            self.events.borrow_mut().push("open");
            if self.at == FailAt::Open {
                fail_write(ws, self.closing);
            } else if self.at == FailAt::Message {
                ws.send_text("trigger").unwrap();
            }
            self.events.borrow_mut().push("open returned");
        }
        fn on_message(&mut self, ws: &WebSocketClient, message: Message<'_>) {
            assert_eq!(message, Message::Text("trigger"));
            self.events.borrow_mut().push("message");
            fail_write(ws, self.closing);
            self.events.borrow_mut().push("message returned");
        }
        fn on_error(&mut self, ws: &WebSocketClient, error: Error) {
            assert!(
                matches!(error, Error::Curl(ref error) if error.code() == curl_sys::CURLE_SEND_ERROR)
            );
            assert_eq!(ws.state(), WebSocketState::Closed);
            assert!(ws.raw_fd().is_none());
            self.events.borrow_mut().push("error");
        }
        fn on_close(&mut self, ws: &WebSocketClient, code: CloseCode, _: &str) {
            assert_eq!(code, CloseCode::ABNORMAL_CLOSURE);
            assert_eq!(ws.state(), WebSocketState::Closed);
            self.events.borrow_mut().push("close");
        }
    }

    struct Healthy(Rc<Cell<usize>>);
    impl WebSocketHandler for Healthy {
        fn on_open(&mut self, ws: &WebSocketClient) {
            ws.send_text("healthy").unwrap();
        }
        fn on_message(&mut self, ws: &WebSocketClient, message: Message<'_>) {
            assert_eq!(message, Message::Text("healthy"));
            self.0.set(self.0.get() + 1);
            ws.close(CloseCode::NORMAL, "").unwrap();
        }
        fn on_error(&mut self, _: &WebSocketClient, error: Error) {
            panic!("sibling connection failed: {error:?}");
        }
    }

    let server = support::PythonServer::start_ws();
    let healthy_server = support::PythonServer::start_ws();
    let mut ev = EventLoop::new().unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut easy = Easy::new();
    easy.url(&server.ws_url()).unwrap();
    let ws = WebSocketClient::connect(
        &ev,
        easy,
        Options::default(),
        Handler {
            at,
            closing,
            events: events.clone(),
        },
    )
    .unwrap();
    let echoes = Rc::new(Cell::new(0));
    let mut easy = Easy::new();
    easy.url(&healthy_server.ws_url()).unwrap();
    let healthy =
        WebSocketClient::connect(&ev, easy, Options::default(), Healthy(echoes.clone())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while ws.state() == WebSocketState::Connecting && Instant::now() < deadline {
        ev.run_once(Duration::from_millis(10)).unwrap();
    }
    if at == FailAt::External {
        assert_eq!(ws.state(), WebSocketState::Open);
        fail_write(&ws, closing);
    }
    while (ws.state() != WebSocketState::Closed || healthy.state() != WebSocketState::Closed)
        && Instant::now() < deadline
    {
        ev.run_once(Duration::from_millis(10)).unwrap();
    }
    assert_eq!(ws.state(), WebSocketState::Closed);
    assert_eq!(healthy.state(), WebSocketState::Closed);
    assert_eq!(echoes.get(), 1);
    assert!(ws.raw_fd().is_none());
    let mut expected = vec!["open", "open returned"];
    if at == FailAt::Message {
        expected.extend(["message", "message returned"]);
    }
    expected.extend(["error", "close"]);
    assert_eq!(*events.borrow(), expected);
    assert!(!ev.run_once(Duration::ZERO).unwrap());
    assert_eq!(*events.borrow(), expected);
}

#[test]
fn direct_send_failures_close_and_notify_once() {
    for at in [FailAt::External, FailAt::Open, FailAt::Message] {
        assert_write_failure_is_reported(at, false);
    }
}

#[test]
fn direct_close_failures_close_and_notify_once() {
    for at in [FailAt::External, FailAt::Open, FailAt::Message] {
        assert_write_failure_is_reported(at, true);
    }
}

#[test]
fn failed_write_cleanup_preserves_callback_panic() {
    struct Handler {
        from_message: bool,
        events: Rc<RefCell<Vec<&'static str>>>,
    }
    impl WebSocketHandler for Handler {
        fn on_open(&mut self, ws: &WebSocketClient) {
            if self.from_message {
                ws.send_text("trigger").unwrap();
            } else {
                fail_write(ws, false);
                panic!("after failed write");
            }
        }
        fn on_message(&mut self, ws: &WebSocketClient, _: Message<'_>) {
            fail_write(ws, false);
            panic!("after failed write");
        }
        fn on_error(&mut self, _: &WebSocketClient, _: Error) {
            self.events.borrow_mut().push("error");
        }
        fn on_close(&mut self, _: &WebSocketClient, code: CloseCode, _: &str) {
            assert_eq!(code, CloseCode::ABNORMAL_CLOSURE);
            self.events.borrow_mut().push("close");
        }
    }

    for from_message in [false, true] {
        let server = support::PythonServer::start_ws();
        let mut ev = EventLoop::new().unwrap();
        let mut easy = Easy::new();
        easy.url(&server.ws_url()).unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let ws = WebSocketClient::connect(
            &ev,
            easy,
            Options::default(),
            Handler {
                from_message,
                events: events.clone(),
            },
        )
        .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while ws.state() != WebSocketState::Closed && Instant::now() < deadline {
                ev.run_once(Duration::from_millis(10)).unwrap();
            }
        }));
        let panic = result.unwrap_err();
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"after failed write"));
        assert_eq!(ws.state(), WebSocketState::Closed);
        assert!(ws.raw_fd().is_none());
        assert_eq!(*events.borrow(), ["error", "close"]);
        assert!(!ev.run_once(Duration::ZERO).unwrap());
    }
}

#[test]
fn rejected_oversized_send_leaves_native_connection_usable() {
    struct Echo(Rc<RefCell<Vec<String>>>);
    impl WebSocketHandler for Echo {
        fn on_open(&mut self, ws: &WebSocketClient) {
            assert!(matches!(
                ws.send_binary(&vec![0; 16 * 1024 * 1024]),
                Err(Error::SendBufferFull)
            ));
            ws.send_text("ok").unwrap();
        }
        fn on_message(&mut self, ws: &WebSocketClient, message: Message<'_>) {
            if let Message::Text(text) = message {
                self.0.borrow_mut().push(text.into());
                ws.close(CloseCode::NORMAL, "").unwrap();
            }
        }
        fn on_error(&mut self, _: &WebSocketClient, error: Error) {
            panic!("unexpected error: {error:?}");
        }
    }
    let server = support::PythonServer::start_ws();
    let mut ev = EventLoop::new().unwrap();
    let mut easy = Easy::new();
    easy.url(&server.ws_url()).unwrap();
    let messages = Rc::new(RefCell::new(Vec::new()));
    let ws = WebSocketClient::connect(
        &ev,
        easy,
        Options::default().max_send_buffer(4),
        Echo(messages.clone()),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while ws.state() != WebSocketState::Closed && Instant::now() < deadline {
        ev.run_once(Duration::from_millis(10)).unwrap();
    }
    assert_eq!(ws.state(), WebSocketState::Closed);
    assert_eq!(*messages.borrow(), ["ok"]);
}
