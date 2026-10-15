mod support;

use curl::easy::Easy;
use curl::event_loop::Error as LoopError;
use curl::http::{HttpClient, HttpClientBuilder};
use curl::websocket::{CloseCode, Message, Options, WebSocketClient, WebSocketHandler};
use curl::{EventLoop, WatchEvents};
use std::cell::{Cell, RefCell};
use std::net::IpAddr;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[test]
fn watch_and_unwatch_dispatch() {
    let mut ev = EventLoop::new().unwrap();
    let mut fds = [0; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) },
        0
    );
    let hits = Rc::new(Cell::new(0));
    let h = hits.clone();
    let token = ev
        .watch(
            fds[0],
            WatchEvents {
                readable: true,
                writable: false,
                error: false,
            },
            move |_| h.set(h.get() + 1),
        )
        .unwrap();
    unsafe {
        libc::write(fds[1], b"x".as_ptr() as _, 1);
    }
    ev.run_once(Duration::from_millis(20)).unwrap();
    assert_eq!(hits.get(), 1);
    ev.unwatch(token).unwrap();
    unsafe {
        libc::write(fds[1], b"y".as_ptr() as _, 1);
    }
    ev.run_once(Duration::from_millis(1)).unwrap();
    assert_eq!(hits.get(), 1);
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}

#[test]
fn timer_updates_timeout_and_fires() {
    let mut ev = EventLoop::new().unwrap();
    let fired = Rc::new(Cell::new(false));
    let f = fired.clone();
    let at = Instant::now() + Duration::from_millis(5);
    ev.add_timer(at, move || f.set(true));
    assert!(ev.next_timeout().unwrap() <= Duration::from_millis(5));
    ev.run_once(Duration::from_millis(30)).unwrap();
    assert!(fired.get());
}

#[test]
fn cancel_timer_prevents_firing_and_stops_loop() {
    let mut ev = EventLoop::new().unwrap();
    let fired = Rc::new(Cell::new(false));
    let f = fired.clone();
    let token = ev.schedule_timeout(Duration::from_millis(10), move || f.set(true));
    assert!(ev.cancel_timer(token));
    assert!(!ev.cancel_timer(token));
    assert!(ev.next_timeout().is_none());
    ev.run().unwrap();
    assert!(!fired.get());
}

#[test]
fn error_variants_and_display_traits() {
    let ev = EventLoop::new().unwrap();
    ev.max_total_connections(16).unwrap();
    ev.max_host_connections(8).unwrap();
    ev.max_connects(16).unwrap();
    ev.pipelining(false, true).unwrap();

    let server = support::PythonServer::start();
    let mut easy = Easy::new();
    easy.url(&server.url()).unwrap();
    let token = ev.add(easy, |_, _| {}).unwrap();
    let recovered = ev.cancel(token).unwrap();
    drop(recovered);

    assert!(matches!(ev.cancel(token), Err(LoopError::NotFound)));
    assert!(matches!(ev.with(token, |_| ()), Err(LoopError::NotFound)));

    let mut fds = [0; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) },
        0
    );
    let watch_token = ev
        .watch(
            fds[0],
            WatchEvents {
                readable: true,
                writable: false,
                error: false,
            },
            |_| {},
        )
        .unwrap();
    assert!(matches!(
        ev.watch(
            fds[0],
            WatchEvents {
                readable: true,
                writable: false,
                error: false,
            },
            |_| {},
        ),
        Err(LoopError::AlreadyRegistered)
    ));
    ev.unwatch(watch_token).unwrap();
    assert!(matches!(ev.unwatch(watch_token), Err(LoopError::NotFound)));
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }

    let err_box: Box<dyn std::error::Error> = Box::new(LoopError::NotFound);
    assert!(err_box.to_string().contains("not found"));
}

#[test]
fn http_client_builder_validation_and_fast_path_handle_reuse() {
    let ev = EventLoop::new().unwrap();
    assert!(HttpClient::builder(&ev).resolve_host("", 80, &["127.0.0.1".parse().unwrap()]).is_err());
    assert!(HttpClient::builder(&ev).resolve_host("example.com", 80, &[]).is_err());

    let s1 = support::PythonServer::start();
    let s2 = support::PythonServer::start();
    let s3 = support::PythonServer::start();
    let port1: u16 = s1
        .url()
        .trim_start_matches("http://127.0.0.1:")
        .trim_end_matches('/')
        .parse()
        .unwrap();
    let port3: u16 = s3
        .url()
        .trim_start_matches("http://127.0.0.1:")
        .trim_end_matches('/')
        .parse()
        .unwrap();

    let mut builder = HttpClientBuilder::new(&ev);
    let wrong_ip: IpAddr = "127.0.0.2".parse().unwrap();
    let right_ip: IpAddr = "127.0.0.1".parse().unwrap();
    builder
        .resolve_host("pinned.test", port1, &[wrong_ip])
        .unwrap()
        .resolve_host("pinned.test", port1, &[right_ip])
        .unwrap()
        .resolve_host("pinned.test", port3, &[right_ip])
        .unwrap()
        .header("X-Fast-Path", "1")
        .timeout(Duration::from_secs(5));
    let client = builder.build();

    let mut ev = ev;
    let mut easy = Easy::new();

    for url in [
        format!("http://pinned.test:{port1}/"),
        s2.url(),
    ] {
        easy.url(&url).unwrap();
        easy.write_function(|data| Ok(data.len())).unwrap();
        let holder = Rc::new(RefCell::new(None));
        let slot = holder.clone();
        client
            .send(easy, move |returned, res| {
                res.unwrap();
                assert_eq!(returned.response_code().unwrap(), 200);
                *slot.borrow_mut() = Some(returned);
            })
            .unwrap();
        ev.run().unwrap();
        easy = holder.borrow_mut().take().unwrap();
    }

    // After Easy::reset(), client_config_id is cleared and defaults are re-applied cleanly.
    easy.reset();
    easy.url(&format!("http://pinned.test:{port3}/")).unwrap();
    easy.write_function(|data| Ok(data.len())).unwrap();
    let done = Rc::new(Cell::new(false));
    let d = done.clone();
    client
        .send(easy, move |returned, res| {
            res.unwrap();
            assert_eq!(returned.response_code().unwrap(), 200);
            d.set(true);
        })
        .unwrap();
    ev.run().unwrap();
    assert!(done.get());
}

#[test]
fn websocket_clone_in_on_open_and_send_close_variant() {
    struct Handler {
        kept_handle: Rc<RefCell<Option<WebSocketClient>>>,
        got_echo: Rc<Cell<bool>>,
        got_close: Rc<Cell<Option<CloseCode>>>,
    }
    impl WebSocketHandler for Handler {
        fn on_open(&mut self, ws: &WebSocketClient) {
            *self.kept_handle.borrow_mut() = Some(ws.clone());
            ws.send_text("direct-from-open").unwrap();
        }
        fn on_message(&mut self, ws: &WebSocketClient, msg: Message<'_>) {
            if let Message::Text(text) = msg {
                assert_eq!(text, "direct-from-open");
                self.got_echo.set(true);
                ws.send(Message::Close {
                    code: CloseCode::NORMAL,
                    reason: "done",
                })
                .unwrap();
            }
        }
        fn on_close(&mut self, _: &WebSocketClient, code: CloseCode, _reason: &str) {
            self.got_close.set(Some(code));
        }
        fn on_error(&mut self, _: &WebSocketClient, err: LoopError) {
            panic!("unexpected ws error: {err:?}");
        }
    }

    let server = support::PythonServer::start_ws();
    let mut ev = EventLoop::new().unwrap();
    let mut easy = Easy::new();
    easy.url(&server.ws_url()).unwrap();

    let kept_handle = Rc::new(RefCell::new(None));
    let got_echo = Rc::new(Cell::new(false));
    let got_close = Rc::new(Cell::new(None));

    let ws = WebSocketClient::connect(
        &ev,
        easy,
        Options::default(),
        Handler {
            kept_handle: kept_handle.clone(),
            got_echo: got_echo.clone(),
            got_close: got_close.clone(),
        },
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut dropped_initial = Some(ws);
    while got_close.get().is_none() && Instant::now() < deadline {
        ev.run_once(Duration::from_millis(10)).unwrap();
        if kept_handle.borrow().is_some() && dropped_initial.is_some() {
            // Drop original handle once on_open has cloned a handle; connection must stay alive.
            dropped_initial.take();
        }
    }
    assert!(got_echo.get());
    assert_eq!(got_close.get(), Some(CloseCode::NORMAL));
}

#[test]
fn websocket_drop_during_connecting_cancels_handshake() {
    struct NeverOpen {
        opened: Rc<Cell<bool>>,
    }
    impl WebSocketHandler for NeverOpen {
        fn on_open(&mut self, _: &WebSocketClient) {
            self.opened.set(true);
        }
    }

    let server = support::PythonServer::start_ws();
    let mut ev = EventLoop::new().unwrap();
    let mut easy = Easy::new();
    easy.url(&server.ws_url()).unwrap();
    let opened = Rc::new(Cell::new(false));
    let ws = WebSocketClient::connect(
        &ev,
        easy,
        Options::default(),
        NeverOpen {
            opened: opened.clone(),
        },
    )
    .unwrap();
    drop(ws);
    ev.run().unwrap();
    assert!(!opened.get());
}

#[test]
fn websocket_abnormal_close_and_sibling_isolation() {
    struct FailingHandler {
        close_code: Rc<Cell<Option<CloseCode>>>,
    }
    impl WebSocketHandler for FailingHandler {
        fn on_close(&mut self, _: &WebSocketClient, code: CloseCode, _reason: &str) {
            self.close_code.set(Some(code));
        }
    }

    struct HealthyHandler {
        echo_count: Rc<Cell<usize>>,
    }
    impl WebSocketHandler for HealthyHandler {
        fn on_open(&mut self, ws: &WebSocketClient) {
            ws.send_text("still-alive").unwrap();
        }
        fn on_message(&mut self, ws: &WebSocketClient, msg: Message<'_>) {
            if let Message::Text(text) = msg {
                assert_eq!(text, "still-alive");
                self.echo_count.set(self.echo_count.get() + 1);
                let _ = ws.close(CloseCode::NORMAL, "");
            }
        }
    }

    let abrupt_server = support::PythonServer::start_ws_close();
    let healthy_server = support::PythonServer::start_ws();
    let mut ev = EventLoop::new().unwrap();

    let close_code = Rc::new(Cell::new(None));
    let mut easy1 = Easy::new();
    easy1.url(&abrupt_server.ws_url()).unwrap();
    let _ws1 = WebSocketClient::connect(
        &ev,
        easy1,
        Options::default(),
        FailingHandler {
            close_code: close_code.clone(),
        },
    )
    .unwrap();

    let echo_count = Rc::new(Cell::new(0));
    let mut easy2 = Easy::new();
    easy2.url(&healthy_server.ws_url()).unwrap();
    let _ws2 = WebSocketClient::connect(
        &ev,
        easy2,
        Options::default(),
        HealthyHandler {
            echo_count: echo_count.clone(),
        },
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    while (close_code.get().is_none() || echo_count.get() == 0) && Instant::now() < deadline {
        ev.run_once(Duration::from_millis(10)).unwrap();
    }
    assert_eq!(close_code.get(), Some(CloseCode::ABNORMAL_CLOSURE));
    assert_eq!(echo_count.get(), 1);
}

#[test]
fn cancelled_earlier_timer_does_not_skip_subsequent_due_timers() {
    let mut ev = EventLoop::new().unwrap();
    let now = Instant::now();
    let fired1 = Rc::new(Cell::new(false));
    let fired2 = Rc::new(Cell::new(false));
    let f1 = fired1.clone();
    let f2 = fired2.clone();

    let t1 = ev.add_timer(now, move || f1.set(true));
    let _t2 = ev.add_timer(now, move || f2.set(true));
    assert!(ev.cancel_timer(t1));

    ev.run_once(Duration::ZERO).unwrap();
    assert!(!fired1.get());
    assert!(fired2.get());
}

#[test]
fn unwatch_inside_watch_callback_removes_watch_cleanly() {
    let mut ev = EventLoop::new().unwrap();
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    unsafe {
        libc::write(fds[1], b"x".as_ptr().cast(), 1);
    }

    let calls = Rc::new(Cell::new(0usize));
    let calls_cb = calls.clone();
    let token_slot = Rc::new(Cell::new(None));
    let token_slot_cb = token_slot.clone();
    let ev_cb = Rc::new(RefCell::new(None::<EventLoop>));

    // Use a pipe and unwatch inside the first readable event
    let token = ev
        .watch(
            fds[0],
            WatchEvents {
                readable: true,
                writable: false,
                error: false,
            },
            {
                let ev_ptr = &ev as *const EventLoop;
                move |_| {
                    calls_cb.set(calls_cb.get() + 1);
                    let tk = token_slot_cb.get().unwrap();
                    // Re-entrant unwatch inside callback must succeed
                    unsafe { (&*ev_ptr).unwatch(tk).unwrap() };
                }
            },
        )
        .unwrap();
    let _ = ev_cb;
    token_slot.set(Some(token));

    ev.run_once(Duration::from_millis(20)).unwrap();
    assert_eq!(calls.get(), 1);
    // Subsequent unwatch must return NotFound (was not re-inserted by callback return)
    assert!(matches!(
        ev.unwatch(token),
        Err(curl::event_loop::Error::NotFound)
    ));

    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}

#[test]
fn http_client_per_request_overrides_do_not_leak_when_recycling_without_reset() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            requests.push(String::from_utf8_lossy(&buf[..n]).into_owned());
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        }
        requests
    });

    let mut ev = EventLoop::new().unwrap();
    let mut builder = HttpClient::builder(&ev);
    builder.header("X-Default", "from-client");
    builder
        .resolve_host("Recycle.Invalid", port, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    let client = builder.build();

    // Request 1: uses HttpClient defaults (fast path after initial stamp)
    let mut easy = Easy::new();
    easy.url(&format!("http://recycle.invalid:{port}/req1")).unwrap();
    let holder: Rc<RefCell<Option<Easy>>> = Rc::new(RefCell::new(None));
    let h1 = holder.clone();
    client
        .send(easy, move |easy, res| {
            res.unwrap();
            *h1.borrow_mut() = Some(easy);
        })
        .unwrap();
    ev.run().unwrap();

    // Request 2: recycle WITHOUT easy.reset(), override X-Default and add X-Custom per-request
    let mut easy = holder.borrow_mut().take().unwrap();
    easy.url(&format!("http://recycle.invalid:{port}/req2")).unwrap();
    let mut custom = curl::easy::List::new();
    custom.append("X-Default: overridden-on-req2").unwrap();
    custom.append("X-Custom: only-req2").unwrap();
    easy.http_headers(custom).unwrap();
    let h2 = holder.clone();
    client
        .send(easy, move |easy, res| {
            res.unwrap();
            *h2.borrow_mut() = Some(easy);
        })
        .unwrap();
    ev.run().unwrap();

    // Request 3: recycle WITHOUT easy.reset() and WITHOUT per-request http_headers;
    // must restore HttpClient's default header ("X-Default: from-client") and not leak Request 2 headers
    let mut easy = holder.borrow_mut().take().unwrap();
    easy.url(&format!("http://recycle.invalid:{port}/req3")).unwrap();
    let h3 = holder.clone();
    client
        .send(easy, move |easy, res| {
            res.unwrap();
            *h3.borrow_mut() = Some(easy);
        })
        .unwrap();
    ev.run().unwrap();

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].contains("X-Default: from-client"));
    assert!(requests[1].contains("X-Default: overridden-on-req2"));
    assert!(requests[1].contains("X-Custom: only-req2"));
    assert!(!requests[1].contains("X-Default: from-client"));
    assert!(requests[2].contains("X-Default: from-client"));
    assert!(!requests[2].contains("X-Default: overridden-on-req2"));
    assert!(!requests[2].contains("X-Custom: only-req2"));
}

#[test]
fn run_until_returns_when_idle() {
    let (done, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut ev = EventLoop::new().unwrap();
        ev.run_until(|_| false).unwrap();

        let timer = ev.schedule_timeout(Duration::from_secs(60), || {});
        assert!(ev.cancel_timer(timer));
        ev.run_until(|_| false).unwrap();
        done.send(()).unwrap();
    });
    result
        .recv_timeout(Duration::from_secs(2))
        .expect("run_until must return when no work remains");
    worker.join().unwrap();
}

#[test]
fn event_loop_as_raw_fd_is_valid() {
    let ev = EventLoop::new().unwrap();
    assert!(ev.as_raw_fd() >= 0);
}



