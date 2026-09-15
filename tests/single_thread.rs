//! Runs without the Rust test harness so its worker thread cannot hide a regression.
mod support;
use curl::easy::Easy;
use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::Duration;

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn thread_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        status
            .lines()
            .find_map(|line| line.strip_prefix("Threads:"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("ps")
            .args(["-M", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .count()
    }
}

fn main() {
    assert_eq!(thread_count(), 1);
    let mut server = Server(Command::new("python3").args(["-u", "-c", r#"
import socket
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    listener.listen(1)
    print(listener.getsockname()[1], flush=True)
    connection, _ = listener.accept()
    with connection:
        request = b''
        while b'\r\n\r\n' not in request:
            chunk = connection.recv(4096)
            if not chunk: raise RuntimeError('incomplete request')
            request += chunk
        assert request.startswith(b'GET / HTTP/1.1\r\n')
        connection.sendall(b'HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nhello!')
"#]).stdout(Stdio::piped()).spawn().unwrap());
    let mut port = String::new();
    BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let mut easy = Easy::new();
    easy.url(&format!("http://127.0.0.1:{}/", port.trim()))
        .unwrap();
    easy.proxy("").unwrap();
    easy.timeout(Duration::from_secs(5)).unwrap();
    let body = Rc::new(RefCell::new(Vec::new()));
    let output = Rc::clone(&body);
    easy.write_function(move |data| {
        output.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    })
    .unwrap();
    support::perform(&mut easy).unwrap();
    assert_eq!(easy.response_code().unwrap(), 200);
    assert_eq!(body.borrow().as_slice(), b"hello!");
    assert!(!curl::Version::get().feature_async_dns());
    assert_eq!(thread_count(), 1);
    drop(easy);
    assert!(server.0.wait().unwrap().success());
    assert_eq!(thread_count(), 1);
    multi_clients_share_one_loop();
    println!("single-thread HTTP and WebSocket regression: passed");
}

fn multi_clients_share_one_loop() {
    use curl::websocket::{Message, Options, WebSocketClient, WebSocketHandler};
    struct Echo {
        count: Rc<RefCell<usize>>,
        expected: String,
    }
    impl WebSocketHandler for Echo {
        fn on_message(&mut self, _: &WebSocketClient, message: Message) {
            if let Message::Text(text) = message {
                assert_eq!(text, self.expected);
                *self.count.borrow_mut() += 1;
            }
        }
        fn on_error(&mut self, _: &WebSocketClient, error: curl::event_loop::Error) {
            panic!("WebSocket failed: {error:?}");
        }
    }
    let http_servers = [
        support::PythonServer::start(),
        support::PythonServer::start(),
    ];
    let ws_servers = [
        support::PythonServer::start_ws(),
        support::PythonServer::start_ws(),
    ];
    let mut lp = curl::EventLoop::new().unwrap();
    let clients = [
        curl::http::HttpClient::new(&lp),
        curl::http::HttpClient::new(&lp),
    ];
    let http_done = Rc::new(RefCell::new(0));
    let ws_done = Rc::new(RefCell::new(0));
    let mut requests = Vec::new();
    let mut sockets = Vec::new();
    for i in 0..2 {
        let mut easy = Easy::new();
        easy.url(&http_servers[i].url()).unwrap();
        easy.write_function(|bytes| Ok(bytes.len())).unwrap();
        let done = http_done.clone();
        requests.push(
            clients[i]
                .send(easy, move |easy, result| {
                    result.unwrap();
                    assert_eq!(easy.response_code().unwrap(), 200);
                    *done.borrow_mut() += 1;
                })
                .unwrap(),
        );
        let mut easy = Easy::new();
        easy.url(&ws_servers[i].ws_url()).unwrap();
        let payload = format!("client-{i}");
        let ws = WebSocketClient::connect(
            &lp,
            easy,
            Options::default(),
            Echo {
                count: ws_done.clone(),
                expected: payload.clone(),
            },
        )
        .unwrap();
        ws.send(Message::Text(payload)).unwrap();
        sockets.push(ws);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while (*http_done.borrow() != 2 || *ws_done.borrow() != 2)
        && std::time::Instant::now() < deadline
    {
        assert_eq!(thread_count(), 1);
        lp.run_once(Duration::from_millis(10)).unwrap();
    }
    assert_eq!(*http_done.borrow(), 2);
    assert_eq!(*ws_done.borrow(), 2);
    assert_eq!(thread_count(), 1);
    println!("multi_clients_share_one_loop: 2 HTTP + 2 WebSocket, Threads=1");
}
