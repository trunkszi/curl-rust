#![allow(dead_code)]
use curl::easy::Easy;
use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;

pub fn run(easy: Easy) -> (Easy, Result<(), curl::event_loop::Error>) {
    let result = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&result);
    let mut event_loop = curl::EventLoop::new().unwrap();
    event_loop
        .add(easy, move |easy, outcome| {
            *slot.borrow_mut() = Some((easy, outcome));
        })
        .unwrap();
    let loop_result = event_loop.run();
    let pair = result.borrow_mut().take();
    match pair {
        Some(pair) => pair,
        None => panic!("event loop completed without callback: {:?}", loop_result),
    }
}
pub fn perform(handle: &mut Easy) -> Result<(), curl::event_loop::Error> {
    let easy = std::mem::replace(handle, Easy::new());
    let (easy, result) = run(easy);
    *handle = easy;
    result
}

pub struct PythonServer {
    child: Child,
    port: u16,
}
impl PythonServer {
    pub fn start() -> Self {
        let mut child = Command::new("python3")
            .arg("tests/support/server.py")
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 and tests/support/server.py are required");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            port: line.trim().parse().unwrap(),
            child,
        }
    }
    pub fn start_https() -> Self {
        let mut child = Command::new("python3")
            .args(["tests/support/server.py", "--https"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 is required");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            port: line.trim().parse().unwrap(),
            child,
        }
    }
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
    pub fn https_url(&self) -> String {
        format!("https://127.0.0.1:{}/", self.port)
    }
}
impl Drop for PythonServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PythonServer {
    pub fn start_ws() -> Self {
        let mut child = Command::new("python3")
            .args(["tests/support/server.py", "--ws"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 is required");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            port: line.trim().parse().unwrap(),
            child,
        }
    }
    pub fn start_ws_active_close() -> Self {
        let mut cmd = Command::new("python3");
        cmd.args(["tests/support/server.py", "--ws", "--active-close"]);
        let mut child = cmd
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 is required");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            port: line.trim().parse().unwrap(),
            child,
        }
    }
    pub fn start_ws_close() -> Self {
        let mut cmd = Command::new("python3");
        cmd.args(["tests/support/server.py", "--ws"])
            .env("WS_CLOSE_AFTER_HANDSHAKE", "1");
        let mut child = cmd
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 is required");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            port: line.trim().parse().unwrap(),
            child,
        }
    }
    pub fn start_wss() -> Self {
        let mut cmd = Command::new("python3");
        cmd.args(["tests/support/server.py", "--wss"])
            .env("WS_TLS", "1");
        let mut child = cmd
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 is required");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            port: line.trim().parse().unwrap(),
            child,
        }
    }
    pub fn start_ws_fragmented() -> Self {
        let mut cmd = Command::new("python3");
        cmd.args(["tests/support/server.py", "--ws"])
            .env("WS_FRAGMENT", "3");
        let mut child = cmd
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 is required");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            port: line.trim().parse().unwrap(),
            child,
        }
    }
    pub fn start_ws_ignore_close() -> Self {
        let mut cmd = Command::new("python3");
        cmd.args(["tests/support/server.py", "--ws"])
            .env("WS_IGNORE_CLOSE", "1");
        let mut child = cmd
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 is required");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            port: line.trim().parse().unwrap(),
            child,
        }
    }
    pub fn start_ws_slow_read() -> Self {
        let mut cmd = Command::new("python3");
        cmd.args(["tests/support/server.py", "--ws"])
            .env("WS_SLOW_READ", "1");
        let mut child = cmd
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 is required");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            port: line.trim().parse().unwrap(),
            child,
        }
    }
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/", self.port)
    }
    pub fn wss_url(&self) -> String {
        format!("wss://127.0.0.1:{}/", self.port)
    }
}
