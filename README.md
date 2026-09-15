# curl-rust

## Platform and threading

This branch supports only `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, and `x86_64-apple-darwin`; unsupported targets fail at compile time. `Easy`, `EventLoop`, `WebSocket`, `List`, and `Form` are `!Send`/`!Sync`. libcurl creates no threads, callbacks are `FnMut`, and callback panics are propagated after the C call.

## DNS and TLS

The application must provide an IP literal or call `Easy::resolve_host` before adding a handle. The library never performs DNS, redirects, Alt-Svc, or DoH connections. TLS is fixed to rustls with native CA verification enabled by default. Setting `cainfo`, `capath`, or `ssl_cainfo_blob` clears native CA; an explicit `ssl_options` value takes precedence.

## EventLoop

Use `EventLoop::add(easy, on_done)`, then `run`, `run_until`, or one iteration with `run_once(max_wait)`. `next_timeout` and `as_raw_fd` allow embedding in an outer epoll/kqueue loop. `watch` registers application file descriptors and `add_timer` schedules callbacks. HTTP and WebSocket transfers share one reactor.

## WebSocket

`WebSocketClient::connect` registers a WebSocket driver on the event loop and returns a client handle whose state starts at `Connecting`; messages are delivered through the configured handler after the handshake. The handler receives `Message::{Text,Binary,Ping,Pong,Close}` and close/error notifications. `Options` defaults are: `max_message_size` 16 MiB, `max_send_buffer` 4 MiB, `close_timeout` 5 seconds, `ping_interval=None`, and `idle_timeout=None`. Keep the `WebSocket` value alive; dropping it closes the connection.

## HttpClient

`HttpClient::new(&event_loop)` stores defaults for DNS resolution, CA path, proxy, timeout, and headers. `send(easy, on_done)` returns a `RequestHandle`; multiple clients and requests can share one `EventLoop`. Explicit options already set on `Easy` take precedence over client defaults. Request handles use weak ownership and return `LoopGone` after the event loop is dropped.

Run examples with `cargo run --example https` and `cargo run --example binance_ws`. The Binance example resolves its host in the application and subscribes to bookTicker, trade, and depth5 streams.

## Tests

Tests require only Python 3 from the standard library and the checked-in self-signed certificate files. No pip packages are needed. Only HTTP(S) and WS(S) are enabled; HTTP/2 is the optional `http2` feature.
