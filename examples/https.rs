//! Simple HTTPS GET
//!
//! This example is a Rust adaptation of the [C example of the same
//! name](https://curl.se/libcurl/c/https.html).

extern crate curl;

use curl::easy::Easy;
use std::io::{Write, stdout};
use std::net::ToSocketAddrs;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut curl = Easy::new();

    curl.url("https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=5")?;
    let ips: Vec<_> = ("api.binance.com", 443)
        .to_socket_addrs()?
        .map(|a| a.ip())
        .collect();
    curl.resolve_host("api.binance.com", 443, &ips)?;
    curl.write_function(|data| {
        stdout().write_all(data).unwrap();
        Ok(data.len())
    })?;

    let event_loop = curl::EventLoop::new().unwrap();
    let client = curl::http::HttpClient::new(&event_loop);
    client
        .send(curl, |_, result| {
            if let Err(error) = result {
                eprintln!("HTTPS request failed: {error:?}");
            }
        })
        .unwrap();
    let mut event_loop = event_loop;
    event_loop.run().unwrap();

    Ok(())
}
