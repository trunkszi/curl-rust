use anyhow::Result;
use curl::easy::Easy;
use std::io::Write;
use std::net::ToSocketAddrs;

const URLS: &[&str] = &[
    "https://www.microsoft.com",
    "https://www.google.com",
    "https://www.amazon.com",
    "https://www.apple.com",
];

fn download(url: &'static str) -> Result<Easy> {
    let mut easy = Easy::new();
    easy.url(url)?;
    let host = url.split("//").nth(1).unwrap().split('/').next().unwrap();
    let ips: Vec<_> = (host, 443).to_socket_addrs()?.map(|a| a.ip()).collect();
    easy.resolve_host(host, 443, &ips)?;
    easy.useragent("curl-rust event loop")?;
    easy.write_function(move |data| {
        std::io::sink().write_all(data).unwrap();
        Ok(data.len())
    })?;
    Ok(easy)
}

fn main() -> Result<()> {
    let mut event_loop = curl::EventLoop::new().unwrap();
    for &url in URLS {
        event_loop
            .add(download(url)?, move |easy, result| {
                println!(
                    "{}: status={:?}, result={:?}",
                    url,
                    easy.response_code(),
                    result
                );
            })
            .unwrap();
    }
    event_loop.run().unwrap();
    Ok(())
}
