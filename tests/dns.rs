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
