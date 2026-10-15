//! Binance combined-stream WebSocket example.
use curl::{
    EventLoop,
    easy::Easy,
    websocket::{CloseCode, Message, Options, WebSocketClient, WebSocketHandler},
};
use std::{
    cell::RefCell,
    net::ToSocketAddrs,
    rc::Rc,
    time::{Duration, Instant},
};

struct Handler {
    counts: Rc<RefCell<[usize; 3]>>,
    done: Rc<RefCell<bool>>,
}
impl WebSocketHandler for Handler {
    fn on_message(&mut self, ws: &WebSocketClient, message: Message<'_>) {
        if let Message::Text(text) = message {
            let mut n = self.counts.borrow_mut();
            if text.contains("@bookTicker") {
                n[0] += 1;
            } else if text.contains("@trade") {
                n[1] += 1;
            } else if text.contains("@depth5") {
                n[2] += 1;
            }
            if n.iter().all(|x| *x > 0) {
                let _ = ws.close(CloseCode::NORMAL, "done");
                *self.done.borrow_mut() = true;
            }
        }
    }
}
fn main() {
    let ips: Vec<_> = ("stream.binance.com", 9443)
        .to_socket_addrs()
        .expect("DNS")
        .map(|a| a.ip())
        .collect();
    let mut easy = Easy::new();
    easy.url("wss://stream.binance.com:9443/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth5@100ms").unwrap();
    easy.resolve_host("stream.binance.com", 9443, &ips).unwrap();
    let counts = Rc::new(RefCell::new([0usize; 3]));
    let done = Rc::new(RefCell::new(false));
    let mut lp = EventLoop::new().unwrap();
    let _ws = WebSocketClient::connect(
        &lp,
        easy,
        Options::default(),
        Handler {
            counts: counts.clone(),
            done: done.clone(),
        },
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    lp.run_until(|_| *done.borrow() || Instant::now() >= deadline)
        .unwrap();
    println!(
        "bookTicker={} trade={} depth5={}",
        counts.borrow()[0],
        counts.borrow()[1],
        counts.borrow()[2]
    );
}
