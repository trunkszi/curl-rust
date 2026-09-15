use std::cell::{Cell, RefCell};
use std::io::Read;
use std::rc::Rc;
use std::str;
use std::time::Duration;

macro_rules! t {
    ($e:expr) => {
        match $e {
            Ok(e) => e,
            Err(e) => panic!("{} failed with {:?}", stringify!($e), e),
        }
    };
}

use curl::easy::{Easy, List, ReadError, WriteError};
use curl::{Error, Version};

use crate::server::Server;
mod server;
mod support;

fn handle() -> Easy {
    let mut e = Easy::new();
    t!(e.timeout(Duration::new(20, 0)));
    e
}

fn sink(data: &[u8]) -> Result<usize, WriteError> {
    Ok(data.len())
}

#[test]
fn get_smoke() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send("HTTP/1.1 200 OK\r\n\r\n");

    let mut handle = handle();
    t!(handle.url(&s.url("/")));
    t!(support::perform(&mut handle));
}

#[test]
fn download_zero_size() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send("HTTP/1.1 200 OK\r\n\r\n");

    let mut handle = handle();
    t!(handle.url(&s.url("/")));
    t!(support::perform(&mut handle));
    assert_eq!(handle.download_size().unwrap(), 0_f64);
}

#[test]
fn download_nonzero_size() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send("HTTP/1.1 200 OK\r\n\r\nHello!");

    let mut handle = handle();
    t!(handle.url(&s.url("/")));
    t!(support::perform(&mut handle));
    assert_eq!(handle.download_size().unwrap(), 6_f64);
}

#[test]
fn upload_zero_size() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send("HTTP/1.1 200 OK\r\n\r\n");

    let mut handle = handle();
    t!(handle.url(&s.url("/")));
    t!(support::perform(&mut handle));
    assert_eq!(handle.upload_size().unwrap(), 0_f64);
}

#[test]
fn upload_nonzero_size() {
    let s = Server::new();
    s.receive(
        "\
         PUT / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         Content-Length: 5\r\n\
         \r\n\
         data\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut data = "data\n".as_bytes();
    let mut list = List::new();
    t!(list.append("Expect:"));
    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.put(true));
    t!(h.in_filesize(5));
    t!(h.upload(true));
    t!(h.http_headers(list));
    {
        t!(h.read_function(move |buf| Ok(data.read(buf).unwrap())));
        t!(support::perform(&mut h));
    }

    assert_eq!(h.upload_size().unwrap(), 5_f64);
}

#[test]
fn get_path() {
    let s = Server::new();
    s.receive(
        "\
         GET /foo HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send("HTTP/1.1 200 OK\r\n\r\n");

    let mut handle = handle();
    t!(handle.url(&s.url("/foo")));
    t!(support::perform(&mut handle));
}

#[test]
fn write_callback() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send("HTTP/1.1 200 OK\r\n\r\nhello!");

    let all = Rc::new(RefCell::new(Vec::<u8>::new()));
    {
        let mut handle = handle();
        t!(handle.url(&s.url("/")));
        let output = Rc::clone(&all);
        t!(handle.write_function(move |data| {
            output.borrow_mut().extend(data);
            Ok(data.len())
        }));
        t!(support::perform(&mut handle));
    }
    assert_eq!(&*all.borrow(), b"hello!");
}

#[test]
fn resolve() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: example.com:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send("HTTP/1.1 200 OK\r\n\r\n");

    let mut handle = handle();
    t!(handle.url(&format!("http://example.com:{}/", s.addr().port())));
    t!(handle.resolve_host(
        "example.com",
        s.addr().port(),
        &["127.0.0.1".parse().unwrap()]
    ));
    t!(support::perform(&mut handle));
}

#[test]
fn progress() {
    let s = Server::new();
    s.receive(
        "\
         GET /foo HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send("HTTP/1.1 200 OK\r\n\r\nHello!");

    let stats = Rc::new(Cell::new((0, 0.0)));
    {
        let mut handle = handle();
        t!(handle.url(&s.url("/foo")));
        t!(handle.progress(true));
        t!(handle.write_function(sink));

        let output = Rc::clone(&stats);
        t!(handle.progress_function(move |_, a, _, _| {
            output.set((output.get().0 + 1, a));
            true
        }));
        t!(support::perform(&mut handle));
    }
    assert!(stats.get().0 > 0);
    assert_eq!(stats.get().1, 6.0);
}

#[test]
fn headers() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
HTTP/1.1 200 OK\r\n\
Foo: bar\r\n\
Bar: baz\r\n\
\r\n
Hello!",
    );

    let headers = Rc::new(RefCell::new(Vec::new()));
    {
        let mut handle = handle();
        t!(handle.url(&s.url("/")));

        let output = Rc::clone(&headers);
        t!(handle.header_function(move |h| {
            output
                .borrow_mut()
                .push(str::from_utf8(h).unwrap().to_string());
            true
        }));
        t!(handle.write_function(sink));
        t!(support::perform(&mut handle));
    }
    assert_eq!(
        *headers.borrow(),
        vec![
            "HTTP/1.1 200 OK\r\n".to_string(),
            "Foo: bar\r\n".to_string(),
            "Bar: baz\r\n".to_string(),
            "\r\n".to_string(),
        ]
    );
}

#[test]
fn fail_on_error() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 401 Not so good\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.fail_on_error(true));
    assert!(support::perform(&mut h).is_err());

    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 401 Not so good\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.fail_on_error(false));
    t!(support::perform(&mut h));
}

#[test]
fn port() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url("http://127.0.0.1/"));
    t!(h.port(s.addr().port()));
    t!(support::perform(&mut h));
}

#[test]
fn proxy() {
    let s = Server::new();
    s.receive(
        "\
         GET http://example.com/ HTTP/1.1\r\n\
         Host: example.com\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url("http://example.com/"));
    t!(h.resolve_host("example.com", 80, &["127.0.0.1".parse().unwrap()]));
    t!(h.proxy(&s.url("/")));
    t!(support::perform(&mut h));
}

#[test]
#[ignore] // fails on newer curl versions? seems benign
fn noproxy() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.proxy(&s.url("/")));
    t!(h.noproxy("127.0.0.1"));
    t!(support::perform(&mut h));
}

#[test]
fn misc() {
    let mut h = handle();
    t!(h.tcp_nodelay(true));
    // t!(h.tcp_keepalive(true));
    // t!(h.tcp_keepidle(Duration::new(3, 0)));
    // t!(h.tcp_keepintvl(Duration::new(3, 0)));
    t!(h.buffer_size(10));

    if Version::get().version_num() >= 0x073e00 {
        // only available on curl 7.62.0 or later:
        t!(h.upload_buffer_size(10));
    }

    t!(h.dns_cache_timeout(Duration::new(1, 0)));
}

#[test]
fn dns_servers() {
    let mut h = handle();
    // Tests are not using a libcurl with c-ares, so this
    // always fails. Test anyway to make sure it returns
    // an error instead of panicing.
    assert!(h.dns_servers("").is_err());
    assert!(h.dns_servers("nonsense").is_err());
    assert!(h.dns_servers("8.8.8.8,8.8.4.4").is_err());
}

#[test]
fn userpass() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Authorization: Basic YmFyOg==\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.username("foo"));
    t!(h.username("bar"));
    t!(support::perform(&mut h));
}

#[test]
fn accept_encoding() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         Accept-Encoding: gzip\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.accept_encoding("gzip"));
    t!(support::perform(&mut h));
}

#[test]
fn put() {
    let s = Server::new();
    s.receive(
        "\
         PUT / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         Content-Length: 5\r\n\
         \r\n\
         data\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut data = "data\n".as_bytes();
    let mut list = List::new();
    t!(list.append("Expect:"));
    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.put(true));
    t!(h.in_filesize(5));
    t!(h.upload(true));
    t!(h.http_headers(list));
    t!(h.read_function(move |buf| Ok(data.read(buf).unwrap())));
    t!(support::perform(&mut h));
}

#[test]
fn post1() {
    let s = Server::new();
    s.receive(
        "\
         POST / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         Content-Length: 5\r\n\
         Content-Type: application/x-www-form-urlencoded\r\n\
         \r\n\
         data\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.post(true));
    t!(h.post_fields_copy(b"data\n"));
    t!(support::perform(&mut h));
}

#[test]
fn post2() {
    let s = Server::new();
    s.receive(
        "\
         POST / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         Content-Length: 5\r\n\
         Content-Type: application/x-www-form-urlencoded\r\n\
         \r\n\
         data\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.post(true));
    t!(h.post_fields_copy(b"data\n"));
    t!(h.write_function(sink));
    t!(support::perform(&mut h));
}

#[test]
fn post3() {
    let s = Server::new();
    s.receive(
        "\
         POST / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         Content-Length: 5\r\n\
         Content-Type: application/x-www-form-urlencoded\r\n\
         \r\n\
         data\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut data = "data\n".as_bytes();
    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.post(true));
    t!(h.post_field_size(5));
    t!(h.read_function(move |buf| Ok(data.read(buf).unwrap())));
    t!(support::perform(&mut h));
}

#[test]
fn referer() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         Referer: foo\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.referer("foo"));
    t!(support::perform(&mut h));
}

#[test]
fn useragent() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         User-Agent: foo\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.useragent("foo"));
    t!(support::perform(&mut h));
}

#[test]
fn custom_headers() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Foo: bar\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut custom = List::new();
    t!(custom.append("Foo: bar"));
    t!(custom.append("Accept:"));
    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.http_headers(custom));
    t!(support::perform(&mut h));
}

#[test]
fn cookie() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         Cookie: foo\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.cookie("foo"));
    t!(support::perform(&mut h));
}

#[test]
fn url_encoding() {
    let mut h = handle();
    assert_eq!(h.url_encode(b"foo"), "foo");
    assert_eq!(h.url_encode(b"foo bar"), "foo%20bar");
    assert_eq!(h.url_encode(b"foo bar\xff"), "foo%20bar%FF");
    assert_eq!(h.url_encode(b""), "");
    assert_eq!(h.url_decode("foo"), b"foo");
    assert_eq!(h.url_decode("foo%20bar"), b"foo bar");
    assert_eq!(h.url_decode("foo%2"), b"foo%2");
    assert_eq!(h.url_decode("foo%xx"), b"foo%xx");
    assert_eq!(h.url_decode("foo%ff"), b"foo\xff");
    assert_eq!(h.url_decode(""), b"");
}

#[test]
fn getters() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.cookie_file("/dev/null"));
    t!(support::perform(&mut h));
    assert_eq!(t!(h.response_code()), 200);
    assert_eq!(t!(h.redirect_count()), 0);
    assert_eq!(t!(h.redirect_url()), None);
    assert_eq!(t!(h.content_type()), None);

    let addr = format!("http://{}/", s.addr());
    assert_eq!(t!(h.effective_url()), Some(&addr[..]));

    // TODO: test this
    // let cookies = t!(h.cookies()).iter()
    //                              .map(|s| s.to_vec())
    //                              .collect::<Vec<_>>();
    // assert_eq!(cookies.len(), 1);
}

#[test]
#[should_panic]
fn panic_in_callback() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.header_function(|_| panic!()));
    t!(support::perform(&mut h));
}

#[test]
fn abort_read() {
    let s = Server::new();
    // 8.7.0 seems to have changed the behavior that curl will call the read
    // function before sending headers (I'm guessing so that it batches
    // everything up into a single write).
    if Version::get().version_num() < 0x080700 {
        s.receive(
            "\
             PUT / HTTP/1.1\r\n\
             Host: 127.0.0.1:$PORT\r\n\
             Accept: */*\r\n\
             Content-Length: 2\r\n\
             \r\n",
        );
        s.send(
            "\
             HTTP/1.1 200 OK\r\n\
             \r\n",
        );
    } else {
        s.receive("");
    }

    let mut h = handle();
    t!(h.url(&s.url("/")));
    t!(h.read_function(|_| Err(ReadError::Abort)));
    t!(h.put(true));
    t!(h.in_filesize(2));
    let mut list = List::new();
    t!(list.append("Expect:"));
    t!(h.http_headers(list));
    let err = support::perform(&mut h).unwrap_err();
    assert!(matches!(err, curl::event_loop::Error::Curl(ref e) if e.is_aborted_by_callback()));
}

#[test]
fn pause_write_then_resume_through_event_loop() {
    let s = Server::new();
    s.receive(
        "\
         GET / HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\na\nb");

    let mut h = handle();
    t!(h.url(&s.url("/")));
    let paused = Rc::new(Cell::new(false));
    let paused_callback = Rc::clone(&paused);
    let body = Rc::new(RefCell::new(Vec::new()));
    let body_callback = Rc::clone(&body);
    t!(h.write_function(move |data| {
        if !paused_callback.replace(true) {
            return Err(WriteError::Pause);
        }
        body_callback.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    }));

    let mut event_loop = curl::EventLoop::new().unwrap();
    let token = event_loop.add(h, |_, result| result.unwrap()).unwrap();
    for _ in 0..20 {
        assert!(event_loop.run_once(Duration::from_millis(50)).unwrap());
        if paused.get() {
            break;
        }
    }
    assert!(paused.get());
    event_loop
        .with(token, |easy| easy.unpause_read())
        .unwrap()
        .unwrap();
    event_loop.run().unwrap();
    assert_eq!(&*body.borrow(), b"a\nb");
}

#[test]
fn check_unix_socket() {
    let s = Server::new_unix();
    s.receive(
        "\
         POST / HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Accept: */*\r\n\
         Content-Length: 5\r\n\
         Content-Type: application/x-www-form-urlencoded\r\n\
         \r\n\
         data\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.unix_socket(s.path()));
    t!(h.url(&s.url("/")));
    t!(h.post(true));
    t!(h.post_fields_copy(b"data\n"));
    t!(support::perform(&mut h));
}

#[test]
fn path_as_is() {
    let s = Server::new();
    s.receive(
        "\
         GET /test/../ HTTP/1.1\r\n\
         Host: 127.0.0.1:$PORT\r\n\
         Accept: */*\r\n\
         \r\n",
    );
    s.send(
        "\
         HTTP/1.1 200 OK\r\n\
         \r\n",
    );

    let mut h = handle();
    t!(h.url(&s.url("/test/../")));
    t!(h.path_as_is(true));
    t!(support::perform(&mut h));

    let addr = format!("http://{}/test/../", s.addr());
    assert_eq!(t!(h.response_code()), 200);
    assert_eq!(t!(h.effective_url()), Some(&addr[..]));
}

#[test]
fn test_connect_timeout() {
    let mut easy = Easy::new();

    // Overflow value test must return an Error
    assert_eq!(
        Error::new(curl_sys::CURLE_BAD_FUNCTION_ARGUMENT),
        easy.connect_timeout(Duration::from_secs(std::u64::MAX))
            .unwrap_err()
    );

    // Valid value
    assert_eq!(
        (),
        easy.connect_timeout(Duration::from_millis(i32::MAX as u64))
            .unwrap()
    );
}

#[test]
fn test_timeout() {
    let mut easy = Easy::new();

    // Overflow value test must return an Error
    assert_eq!(
        Error::new(curl_sys::CURLE_BAD_FUNCTION_ARGUMENT),
        easy.timeout(Duration::from_secs(std::u64::MAX))
            .unwrap_err()
    );

    // Valid value
    assert_eq!(
        (),
        easy.timeout(Duration::from_millis(i32::MAX as u64))
            .unwrap()
    );
}

#[test]
fn event_loop_python_get() {
    let service = support::PythonServer::start();
    let mut easy = Easy::new();
    easy.url(&service.url()).unwrap();
    let body = Rc::new(RefCell::new(Vec::new()));
    let output = Rc::clone(&body);
    easy.write_function(move |data| {
        output.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    })
    .unwrap();
    let (_easy, result) = support::run(easy);
    result.unwrap();
    assert_eq!(&*body.borrow(), b"ok");
}

#[test]
fn resolve_host_accepts_typed_addresses() {
    let mut easy = Easy::new();
    easy.resolve_host("example.test", 443, &["127.0.0.1".parse().unwrap()])
        .unwrap();
    easy.url("https://example.test/").unwrap();
}

#[test]
fn python_https_get_with_custom_ca() {
    let service = support::PythonServer::start_https();
    let mut easy = Easy::new();
    easy.url(&service.https_url()).unwrap();
    easy.cainfo("tests/support/ca.pem").unwrap();
    let body = Rc::new(RefCell::new(Vec::new()));
    let out = Rc::clone(&body);
    easy.write_function(move |data| {
        out.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    })
    .unwrap();
    support::perform(&mut easy).unwrap();
    assert_eq!(&*body.borrow(), b"ok");
}

#[test]
#[should_panic]
fn event_loop_propagates_callback_panic() {
    let server = Server::new();
    server.receive("GET / HTTP/1.1\r\nHost: 127.0.0.1:$PORT\r\nAccept: */*\r\n\r\n");
    server.send("HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx");
    let mut easy = Easy::new();
    easy.url(&server.url("/")).unwrap();
    easy.write_function(|_| panic!("callback panic")).unwrap();
    let mut event_loop = curl::EventLoop::new().unwrap();
    event_loop.add(easy, |_, _| {}).unwrap();
    let _ = event_loop.run();
}

use curl::websocket::{
    CloseCode as WsCode, Message as WsMessage, Options as WsOptions, WebSocketClient as WsClient,
    WebSocketHandler as WsHandler,
};

#[derive(Default)]
struct WsObservations {
    opened: usize,
    messages: Vec<WsMessage>,
    closes: Vec<(WsCode, String)>,
    errors: Vec<curl::event_loop::Error>,
}
struct ObserveWs(Rc<RefCell<WsObservations>>);
impl WsHandler for ObserveWs {
    fn on_open(&mut self, _: &WsClient) {
        self.0.borrow_mut().opened += 1;
    }
    fn on_message(&mut self, _: &WsClient, message: WsMessage) {
        self.0.borrow_mut().messages.push(message);
    }
    fn on_close(&mut self, _: &WsClient, code: WsCode, reason: String) {
        self.0.borrow_mut().closes.push((code, reason));
    }
    fn on_error(&mut self, _: &WsClient, error: curl::event_loop::Error) {
        self.0.borrow_mut().errors.push(error);
    }
}
fn observed_ws(
    lp: &curl::EventLoop,
    easy: Easy,
    options: WsOptions,
) -> (WsClient, Rc<RefCell<WsObservations>>) {
    let observed = Rc::new(RefCell::new(WsObservations::default()));
    let ws = WsClient::connect(lp, easy, options, ObserveWs(observed.clone())).unwrap();
    (ws, observed)
}
fn drive_ws_until(lp: &mut curl::EventLoop, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !done() && std::time::Instant::now() < deadline {
        lp.run_once(Duration::from_millis(10)).unwrap();
    }
    assert!(done(), "WebSocket operation did not finish before deadline");
}

#[test]
fn websocket_handshake_delivers_connection_and_can_send() {
    let service = support::PythonServer::start_ws();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (ws, observed) = observed_ws(&lp, easy, WsOptions::default());
    ws.send(WsMessage::Text("hello".into())).unwrap();
    drive_ws_until(&mut lp, || !observed.borrow().messages.is_empty());
    let events = observed.borrow();
    assert_eq!(events.opened, 1);
    assert_eq!(events.messages, vec![WsMessage::Text("hello".into())]);
    assert!(events.closes.is_empty());
}

#[test]
fn websocket_peer_tcp_close_invokes_on_close_once() {
    let service = support::PythonServer::start_ws_close();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (_ws, observed) = observed_ws(&lp, easy, WsOptions::default());
    drive_ws_until(&mut lp, || !observed.borrow().closes.is_empty());
    for _ in 0..3 {
        lp.run_once(Duration::ZERO).unwrap();
    }
    assert_eq!(observed.borrow().closes.len(), 1);
}

#[test]
fn websocket_message_size_limit_is_enforced() {
    let bad = support::PythonServer::start_ws();
    let good = support::PythonServer::start_ws();
    let mut bad_easy = Easy::new();
    bad_easy.url(&bad.ws_url()).unwrap();
    let mut good_easy = Easy::new();
    good_easy.url(&good.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (bad_ws, rejected) = observed_ws(&lp, bad_easy, WsOptions::default().max_message_size(2));
    let (good_ws, healthy) = observed_ws(&lp, good_easy, WsOptions::default());
    bad_ws.send(WsMessage::Text("oversized".into())).unwrap();
    drive_ws_until(&mut lp, || !rejected.borrow().closes.is_empty());
    assert!(matches!(
        rejected.borrow().errors.as_slice(),
        [curl::event_loop::Error::MessageTooLarge]
    ));
    assert_eq!(rejected.borrow().closes[0].0, WsCode(1009));
    good_ws
        .send(WsMessage::Text("healthy after rejection".into()))
        .unwrap();
    drive_ws_until(&mut lp, || !healthy.borrow().messages.is_empty());
    assert_eq!(
        healthy.borrow().messages,
        vec![WsMessage::Text("healthy after rejection".into())]
    );
}

#[test]
fn websocket_wss_echo_with_custom_ca() {
    let service = support::PythonServer::start_wss();
    let mut easy = Easy::new();
    easy.url(&service.wss_url()).unwrap();
    easy.cainfo("tests/support/ca.pem").unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (ws, observed) = observed_ws(&lp, easy, WsOptions::default());
    let payload = WsMessage::Text("hello".into());
    let expected = payload.clone();
    ws.send(payload).unwrap();
    drive_ws_until(&mut lp, || !observed.borrow().messages.is_empty());
    assert_eq!(observed.borrow().messages, vec![expected]);
}

#[test]
fn websocket_fragmented_message_is_reassembled() {
    let service = support::PythonServer::start_ws_fragmented();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (ws, observed) = observed_ws(&lp, easy, WsOptions::default());
    let payload = WsMessage::Text("fragmented".into());
    let expected = payload.clone();
    ws.send(payload).unwrap();
    drive_ws_until(&mut lp, || !observed.borrow().messages.is_empty());
    assert_eq!(observed.borrow().messages, vec![expected]);
}

#[test]
fn websocket_binary_roundtrip() {
    let service = support::PythonServer::start_ws();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (ws, observed) = observed_ws(&lp, easy, WsOptions::default());
    let payload = WsMessage::Binary(
        (0..1024 * 1024)
            .map(|i| (i as u8).wrapping_mul(31))
            .collect(),
    );
    let expected = payload.clone();
    ws.send(payload).unwrap();
    drive_ws_until(&mut lp, || !observed.borrow().messages.is_empty());
    assert_eq!(observed.borrow().messages, vec![expected]);
}

#[test]
fn websocket_ping_pong_visible() {
    let service = support::PythonServer::start_ws();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (ws, observed) = observed_ws(&lp, easy, WsOptions::default());
    let payload = WsMessage::Ping(b"x".to_vec());
    let expected = WsMessage::Pong(b"x".to_vec());
    ws.send(payload).unwrap();
    drive_ws_until(&mut lp, || !observed.borrow().messages.is_empty());
    assert_eq!(observed.borrow().messages, vec![expected]);
}

#[test]
fn websocket_close_handshake_completes() {
    let service = support::PythonServer::start_ws();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (ws, observed) = observed_ws(
        &lp,
        easy,
        WsOptions::default().close_timeout(Duration::from_millis(200)),
    );
    drive_ws_until(&mut lp, || observed.borrow().opened == 1);
    ws.close(WsCode::NORMAL, "bye").unwrap();
    drive_ws_until(&mut lp, || !observed.borrow().closes.is_empty());
    assert_eq!(observed.borrow().closes.len(), 1);
    assert_eq!(observed.borrow().closes[0], (WsCode::NORMAL, "bye".into()));
}

#[test]
fn websocket_close_timeout_when_peer_ignores() {
    let service = support::PythonServer::start_ws_ignore_close();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (ws, observed) = observed_ws(
        &lp,
        easy,
        WsOptions::default().close_timeout(Duration::from_millis(200)),
    );
    drive_ws_until(&mut lp, || observed.borrow().opened == 1);
    ws.close(WsCode::NORMAL, "bye").unwrap();
    drive_ws_until(&mut lp, || !observed.borrow().closes.is_empty());
    assert_eq!(observed.borrow().closes.len(), 1);
    assert!(observed.borrow().closes[0].1.contains("timeout"));
}

#[test]
fn websocket_idle_timeout_closes() {
    let service = support::PythonServer::start_ws_ignore_close();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (_ws, observed) = observed_ws(
        &lp,
        easy,
        WsOptions::default().idle_timeout(Some(Duration::from_millis(300))),
    );
    drive_ws_until(&mut lp, || !observed.borrow().closes.is_empty());
    assert_eq!(observed.borrow().closes.len(), 1);
}
#[test]
fn http_and_websocket_share_event_loop() {
    let http = support::PythonServer::start();
    let service = support::PythonServer::start_ws();
    let mut http_easy = Easy::new();
    http_easy.url(&http.url()).unwrap();
    http_easy.write_function(|d| Ok(d.len())).unwrap();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let done = Rc::new(Cell::new(false));
    let flag = done.clone();
    let client = curl::http::HttpClient::new(&lp);
    let _request = client
        .send(http_easy, move |_, result| {
            result.unwrap();
            flag.set(true);
        })
        .unwrap();
    let (ws, observed) = observed_ws(&lp, easy, WsOptions::default());
    ws.send(WsMessage::Text("hello".into())).unwrap();
    drive_ws_until(&mut lp, || {
        done.get() && !observed.borrow().messages.is_empty()
    });
    assert_eq!(
        observed.borrow().messages,
        vec![WsMessage::Text("hello".into())]
    );
}
#[test]
fn websocket_send_buffer_full() {
    let service = support::PythonServer::start_ws_slow_read();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let lp = curl::EventLoop::new().unwrap();
    let (ws, _) = observed_ws(&lp, easy, WsOptions::default().max_send_buffer(64 * 1024));
    assert!(matches!(
        ws.send(WsMessage::Binary(vec![7; 128 * 1024])),
        Err(curl::event_loop::Error::SendBufferFull)
    ));
}

#[test]
fn websocket_connect_returns_live_handle_and_queues_before_open() {
    use curl::websocket::{Message, Options, WebSocketClient, WebSocketHandler, WebSocketState};
    struct Handler(Rc<RefCell<Vec<String>>>);
    impl WebSocketHandler for Handler {
        fn on_open(&mut self, ws: &WebSocketClient) {
            assert_eq!(ws.state(), WebSocketState::Open);
            self.0.borrow_mut().push("open".into());
        }
        fn on_message(&mut self, _: &WebSocketClient, message: Message) {
            if let Message::Text(text) = message {
                self.0.borrow_mut().push(text);
            }
        }
    }
    let server = support::PythonServer::start_ws();
    let mut easy = Easy::new();
    easy.url(&server.ws_url()).unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut lp = curl::EventLoop::new().unwrap();
    let ws =
        WebSocketClient::connect(&lp, easy, Options::default(), Handler(events.clone())).unwrap();
    assert_eq!(ws.state(), WebSocketState::Connecting);
    ws.send(Message::Text("queued before handshake".into()))
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while events.borrow().len() < 2 && std::time::Instant::now() < deadline {
        lp.run_once(Duration::from_millis(10)).unwrap();
    }
    assert_eq!(*events.borrow(), vec!["open", "queued before handshake"]);
}

#[test]
fn http_client_request_handle_does_not_keep_event_loop_alive() {
    let lp = curl::EventLoop::new().unwrap();
    let client = curl::http::HttpClient::new(&lp);
    let mut easy = Easy::new();
    easy.url("http://127.0.0.1:1/").unwrap();
    let handle = client.send(easy, |_, _| {}).unwrap();
    drop(lp);
    assert!(matches!(
        handle.with(|_| ()),
        Err(curl::event_loop::Error::LoopGone)
    ));
}

#[test]
fn http_client_sends_request_through_shared_event_loop() {
    let server = Server::new();
    server.receive("GET / HTTP/1.1\r\nHost: 127.0.0.1:$PORT\r\nAccept: */*\r\n\r\n");
    server.send("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    let mut easy = Easy::new();
    easy.url(&server.url("/")).unwrap();
    let body = Rc::new(RefCell::new(Vec::new()));
    let out = body.clone();
    easy.write_function(move |d| {
        out.borrow_mut().extend_from_slice(d);
        Ok(d.len())
    })
    .unwrap();
    let lp = curl::EventLoop::new().unwrap();
    let client = curl::http::HttpClient::new(&lp);
    let done = Rc::new(RefCell::new(None));
    let slot = done.clone();
    let _handle = client
        .send(easy, move |_, result| *slot.borrow_mut() = Some(result))
        .unwrap();
    let mut lp = lp;
    lp.run().unwrap();
    assert!(done.borrow().as_ref().unwrap().is_ok());
    assert_eq!(body.borrow().as_slice(), b"ok");
}

#[test]
fn websocket_handle_reports_loop_gone_after_event_loop_drop() {
    struct Noop;
    impl curl::websocket::WebSocketHandler for Noop {}
    let lp = curl::EventLoop::new().unwrap();
    let mut easy = Easy::new();
    easy.url("ws://127.0.0.1:1/").unwrap();
    let ws = curl::websocket::WebSocketClient::connect(
        &lp,
        easy,
        curl::websocket::Options::default(),
        Noop,
    )
    .unwrap();
    drop(lp);
    assert!(matches!(
        ws.send(curl::websocket::Message::Ping(vec![])),
        Err(curl::event_loop::Error::LoopGone)
    ));
}

#[test]
fn websocket_ping_interval_receives_pongs() {
    let service = support::PythonServer::start_ws();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (_ws, observed) = observed_ws(
        &lp,
        easy,
        WsOptions::default().ping_interval(Some(Duration::from_millis(100))),
    );
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while std::time::Instant::now() < deadline
        && observed
            .borrow()
            .messages
            .iter()
            .filter(|m| matches!(m, WsMessage::Pong(_)))
            .count()
            < 2
    {
        lp.run_once(Duration::from_millis(20)).unwrap();
    }
    assert!(
        observed
            .borrow()
            .messages
            .iter()
            .filter(|m| matches!(m, WsMessage::Pong(_)))
            .count()
            >= 2
    );
}

#[test]
fn websocket_peer_active_close_is_acknowledged() {
    let service = support::PythonServer::start_ws_active_close();
    let mut easy = Easy::new();
    easy.url(&service.ws_url()).unwrap();
    let mut lp = curl::EventLoop::new().unwrap();
    let (_ws, observed) = observed_ws(&lp, easy, WsOptions::default());
    drive_ws_until(&mut lp, || !observed.borrow().closes.is_empty());
    assert_eq!(
        observed.borrow().closes[0],
        (WsCode(1001), "going away".into())
    );
}
