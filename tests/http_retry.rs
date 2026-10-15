use curl::EventLoop;
use curl::easy::{Easy, List};
use curl::event_loop::Error;
use curl::http::HttpClient;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

#[test]
fn rejected_request_retains_explicit_options_for_retry() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);
        assert!(request.contains("X-Retry: explicit"));
        assert!(!request.contains("X-Retry: default"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });

    let mut ev = EventLoop::new().unwrap();
    let client = HttpClient::new(&ev)
        .header("X-Retry", "default")
        .proxy("http://unprepared-proxy.invalid:8080");
    let mut easy = Easy::new();
    easy.resolve_host("right.invalid", port, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    easy.url(&format!("http://wrong.invalid:{port}/")).unwrap();
    easy.proxy("").unwrap();
    easy.timeout(Duration::from_secs(2)).unwrap();
    let mut headers = List::new();
    headers.append("X-Retry: explicit").unwrap();
    easy.http_headers(headers).unwrap();

    let err = client
        .send(easy, |_, _| panic!("rejected request ran"))
        .unwrap_err();
    assert!(matches!(err.error, Error::UnresolvedHost));
    let mut easy = err.easy;
    easy.url(&format!("http://right.invalid:{port}/")).unwrap();
    let done = Rc::new(Cell::new(false));
    let done_cb = done.clone();
    client
        .send(easy, move |_, result| {
            result.unwrap();
            done_cb.set(true);
        })
        .unwrap();
    ev.run().unwrap();
    assert!(done.get());
    server.join().unwrap();
}

#[test]
fn default_application_error_retains_explicit_dns_for_retry() {
    let ev = EventLoop::new().unwrap();
    let client = HttpClient::new(&ev).header("X-Invalid", "contains\0nul");
    let mut easy = Easy::new();
    easy.url("http://right.invalid/").unwrap();
    easy.resolve_host("right.invalid", 80, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    let err = client.send(easy, |_, _| {}).unwrap_err();
    assert!(matches!(err.error, Error::Curl(_)));

    let client = HttpClient::new(&ev);
    let handle = client.send(err.easy, |_, _| {}).unwrap();
    handle.cancel().unwrap();
}
