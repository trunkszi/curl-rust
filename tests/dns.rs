use curl::easy::Easy;

#[test]
fn unprepared_domain_is_rejected() {
    let mut easy = Easy::new();
    easy.url("http://example.invalid/").unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let err = loop_.add(easy, |_, _| {}).unwrap_err();
    assert!(matches!(err.error, curl::event_loop::Error::UnresolvedHost));
}

#[test]
fn ip_literal_is_accepted() {
    let mut easy = Easy::new();
    easy.url("http://127.0.0.1:1/").unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let token = loop_.add(easy, |_, _| {}).unwrap();
    let _ = loop_.cancel(token);
}

#[test]
fn ipv6_literal_is_accepted() {
    let mut easy = Easy::new();
    easy.url("http://[::1]:1/").unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let token = loop_.add(easy, |_, _| {}).unwrap();
    let _ = loop_.cancel(token);
}

#[test]
fn ipv6_zone_literal_is_accepted() {
    let mut easy = Easy::new();
    easy.url("http://[fe80::1%25lo]:1/").unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let token = loop_.add(easy, |_, _| {}).unwrap();
    let _ = loop_.cancel(token);
}

#[test]
fn resolved_domain_is_accepted() {
    let mut easy = Easy::new();
    easy.url("http://example.invalid:80/").unwrap();
    easy.resolve_host("example.invalid", 80, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let token = loop_.add(easy, |_, _| {}).unwrap();
    let _ = loop_.cancel(token);
}

#[test]
fn url_port_override_is_validated() {
    let mut easy = Easy::new();
    easy.url("http://example.invalid:80/").unwrap();
    easy.port(8080).unwrap();
    easy.resolve_host("example.invalid", 8080, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let token = loop_.add(easy, |_, _| {}).unwrap();
    let _ = loop_.cancel(token);
}

#[test]
fn unprepared_proxy_is_rejected() {
    let mut easy = Easy::new();
    easy.url("http://127.0.0.1:1/").unwrap();
    easy.proxy("http://proxy.invalid:8080").unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let err = loop_.add(easy, |_, _| {}).unwrap_err();
    assert!(matches!(err.error, curl::event_loop::Error::UnresolvedHost));
}

#[test]
fn proxy_without_port_uses_default_1080() {
    let mut easy = Easy::new();
    easy.url("http://127.0.0.1:1/").unwrap();
    easy.proxy("http://proxy.invalid").unwrap();
    easy.resolve_host("proxy.invalid", 1080, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let token = loop_.add(easy, |_, _| {}).unwrap();
    let _ = loop_.cancel(token);
}

#[test]
fn resolved_proxy_uses_explicit_proxy_port() {
    let mut easy = Easy::new();
    easy.url("http://127.0.0.1:1/").unwrap();
    easy.proxy("http://proxy.invalid").unwrap();
    easy.proxy_port(8080).unwrap();
    easy.resolve_host("proxy.invalid", 8080, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let token = loop_.add(easy, |_, _| {}).unwrap();
    let _ = loop_.cancel(token);
}

#[test]
fn reset_clears_dns_preloads() {
    let mut easy = Easy::new();
    easy.resolve_host("example.invalid", 80, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    easy.url("http://example.invalid/").unwrap();
    easy.reset();
    easy.url("http://example.invalid/").unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let err = loop_.add(easy, |_, _| {}).unwrap_err();
    assert!(matches!(err.error, curl::event_loop::Error::UnresolvedHost));
}

#[test]
fn dropping_active_loop_is_safe() {
    let mut easy = Easy::new();
    easy.url("http://127.0.0.1:1/").unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    loop_.add(easy, |_, _| {}).unwrap();
}

#[test]
fn empty_resolve_host_or_addrs_is_rejected() {
    let mut easy = Easy::new();
    assert!(easy.resolve_host("", 80, &["127.0.0.1".parse().unwrap()]).is_err());
    assert!(easy.resolve_host("example.invalid", 80, &[]).is_err());
}

#[test]
fn multiple_resolve_host_calls_accumulate() {
    use std::cell::Cell;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::rc::Rc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        }
    });

    let mut easy = Easy::new();
    easy.resolve_host("first.invalid", port, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    easy.resolve_host("second.invalid", port, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    easy.url(&format!("http://first.invalid:{port}/")).unwrap();

    let mut loop_ = curl::EventLoop::new().unwrap();
    let holder = Rc::new(std::cell::RefCell::new(None));
    let h1 = holder.clone();
    loop_
        .add(easy, move |easy, res| {
            res.unwrap();
            assert_eq!(easy.response_code().unwrap(), 200);
            *h1.borrow_mut() = Some(easy);
        })
        .unwrap();
    loop_.run().unwrap();

    // Reuse the same Easy handle to connect to the second accumulated host without re-resolving
    let mut easy = holder.borrow_mut().take().unwrap();
    easy.url(&format!("http://second.invalid:{port}/")).unwrap();
    let done = Rc::new(Cell::new(false));
    let done2 = done.clone();
    loop_
        .add(easy, move |easy, res| {
            res.unwrap();
            assert_eq!(easy.response_code().unwrap(), 200);
            done2.set(true);
        })
        .unwrap();
    loop_.run().unwrap();
    assert!(done.get());
    server.join().unwrap();
}

#[test]
fn duplicate_resolve_host_deduplicates_and_replaces() {
    use std::cell::Cell;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::rc::Rc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 512];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .unwrap();
    });

    let mut easy = Easy::new();
    let unreachable_ip = "192.0.2.1".parse().unwrap();
    let loopback_ip = "127.0.0.1".parse().unwrap();
    easy.resolve_host("example.invalid", port, &[unreachable_ip, unreachable_ip])
        .unwrap();
    // Second call for the same (host, port) must replace the unreachable IP with 127.0.0.1
    easy.resolve_host("example.invalid", port, &[loopback_ip, loopback_ip])
        .unwrap();
    easy.url(&format!("http://example.invalid:{port}/")).unwrap();

    let mut loop_ = curl::EventLoop::new().unwrap();
    let ok = Rc::new(Cell::new(false));
    let ok2 = ok.clone();
    loop_
        .add(easy, move |easy, res| {
            res.unwrap();
            assert_eq!(easy.response_code().unwrap(), 200);
            ok2.set(true);
        })
        .unwrap();
    loop_.run().unwrap();
    assert!(ok.get());
    server.join().unwrap();
}

#[test]
fn schemeless_url_with_colon_slash_slash_in_query_does_not_bypass_validation() {
    let mut easy = Easy::new();
    easy.url("unprepared.invalid:80/?redirect=http://127.0.0.1:80")
        .unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let err = loop_.add(easy, |_, _| {}).unwrap_err();
    assert!(matches!(err.error, curl::event_loop::Error::UnresolvedHost));
}

#[test]
fn inline_proxy_url_port_takes_precedence_over_proxy_port() {
    let mut easy = Easy::new();
    easy.url("http://127.0.0.1:1/").unwrap();
    easy.proxy("http://proxy.invalid:1080").unwrap();
    easy.proxy_port(8080).unwrap();

    // libcurl uses :1080 from the URL, regardless of CURLOPT_PROXYPORT.
    easy.resolve_host("proxy.invalid", 8080, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    let loop_ = curl::EventLoop::new().unwrap();
    let err = loop_.add(easy, |_, _| {}).unwrap_err();
    assert!(matches!(err.error, curl::event_loop::Error::UnresolvedHost));

    let mut easy = err.easy;
    easy.resolve_host("proxy.invalid", 1080, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    let token = loop_.add(easy, |_, _| {}).unwrap();
    let _ = loop_.cancel(token);
}

#[test]
fn inline_proxy_url_port_is_used_for_actual_connection() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).starts_with("GET http://127.0.0.1:1/"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });

    let mut easy = Easy::new();
    easy.url("http://127.0.0.1:1/").unwrap();
    easy.proxy(&format!("http://proxy.invalid:{port}")).unwrap();
    easy.proxy_port(if port == 8080 { 8081 } else { 8080 })
        .unwrap();
    easy.resolve_host("proxy.invalid", port, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    easy.timeout(std::time::Duration::from_secs(2)).unwrap();
    let mut loop_ = curl::EventLoop::new().unwrap();
    loop_
        .add(easy, |easy, result| {
            result.unwrap();
            assert_eq!(easy.response_code().unwrap(), 200);
        })
        .unwrap();
    loop_.run().unwrap();
    server.join().unwrap();
}

