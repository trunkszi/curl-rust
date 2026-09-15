//! Multi-request HTTP handles sharing one event loop.
use crate::easy::{Easy, List};
use crate::event_loop::{Error, EventLoop, LoopInner, Token};
use std::marker::PhantomData;
use std::net::IpAddr;
use std::rc::{Rc, Weak};
use std::time::Duration;

#[derive(Debug, Default, Clone)]
struct Defaults {
    resolves: Vec<(String, u16, Vec<IpAddr>)>,
    cainfo: Option<String>,
    proxy: Option<String>,
    timeout: Option<Duration>,
    headers: Vec<(String, String)>,
}

/// HTTP client configuration bound weakly to an event loop.
#[derive(Debug)]
pub struct HttpClient {
    loop_: Weak<LoopInner>,
    defaults: Defaults,
    _marker: PhantomData<*mut ()>,
}

impl HttpClient {
    /// Creates a client sharing `loop_`.
    pub fn new(loop_: &EventLoop) -> Self {
        Self {
            loop_: Rc::downgrade(&loop_.inner),
            defaults: Defaults::default(),
            _marker: PhantomData,
        }
    }
    /// Public interface for this event-loop operation.
    pub fn resolve_host(mut self, host: &str, port: u16, addrs: &[IpAddr]) -> Self {
        self.defaults
            .resolves
            .push((host.into(), port, addrs.to_vec()));
        self
    }

    /// Public interface for this event-loop operation.
    pub fn cainfo(mut self, path: &str) -> Self {
        self.defaults.cainfo = Some(path.into());
        self
    }
    /// Public interface for this event-loop operation.
    pub fn proxy(mut self, url: &str) -> Self {
        self.defaults.proxy = Some(url.into());
        self
    }

    /// Public interface for this event-loop operation.
    pub fn timeout(mut self, d: Duration) -> Self {
        self.defaults.timeout = Some(d);
        self
    }

    /// Public interface for this event-loop operation.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.defaults.headers.push((name.into(), value.into()));
        self
    }

    /// Public interface for this event-loop operation.
    pub fn send<F>(
        &self,
        mut easy: Easy,
        on_done: F,
    ) -> Result<RequestHandle, crate::event_loop::AddError>
    where
        F: FnOnce(Easy, Result<(), Error>) + 'static,
    {
        macro_rules! apply {
            ($call:expr) => {
                if let Err(error) = $call {
                    return Err(crate::event_loop::AddError {
                        easy,
                        error: Error::Curl(error),
                    });
                }
            };
        }
        for (host, port, addrs) in &self.defaults.resolves {
            apply!(easy.resolve_host(host, *port, addrs));
        }
        if let Some(path) = &self.defaults.cainfo {
            if !easy.inner.explicit_cainfo {
                apply!(easy.cainfo(path));
            }
        }
        if let Some(proxy) = &self.defaults.proxy {
            if !easy.inner.explicit_proxy {
                apply!(easy.proxy(proxy));
            }
        }
        if let Some(timeout) = self.defaults.timeout {
            if !easy.inner.explicit_timeout {
                apply!(easy.timeout(timeout));
            }
        }
        if !self.defaults.headers.is_empty() {
            let mut list = List::new();
            let mut names = std::collections::HashSet::new();
            if let Some(existing) = easy.inner.header_list.as_ref() {
                for raw in existing {
                    let text = String::from_utf8_lossy(raw);
                    if let Some(name) = text
                        .split_once(':')
                        .map(|(n, _)| n.trim().to_ascii_lowercase())
                    {
                        names.insert(name);
                    }
                    apply!(list.append(&text));
                }
            }
            for (n, v) in &self.defaults.headers {
                if names.insert(n.trim().to_ascii_lowercase()) {
                    apply!(list.append(&format!("{n}: {v}")));
                }
            }
            apply!(easy.http_headers(list));
        }
        let loop_ = match self.loop_.upgrade() {
            Some(loop_) => loop_,
            None => {
                return Err(crate::event_loop::AddError {
                    easy,
                    error: Error::LoopGone,
                });
            }
        };
        let token = loop_
            .enqueue(easy, Some(Box::new(on_done)), None)
            .map_err(|e| e)?;
        Ok(RequestHandle {
            loop_: Rc::downgrade(&loop_),
            token,
            _marker: PhantomData,
        })
    }
}
/// A cancellable request handle.
pub struct RequestHandle {
    loop_: Weak<LoopInner>,
    token: Token,
    _marker: PhantomData<*mut ()>,
}

impl std::fmt::Debug for RequestHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestHandle")
            .field("token", &self.token)
            .finish()
    }
}

impl RequestHandle {
    /// Public interface for this event-loop operation.
    pub fn cancel(self) -> Result<Easy, Error> {
        self.loop_
            .upgrade()
            .ok_or(Error::LoopGone)?
            .cancel(self.token)
    }
    /// Public interface for this event-loop operation.
    pub fn with<F, R>(&self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Easy) -> R,
    {
        self.loop_
            .upgrade()
            .ok_or(Error::LoopGone)?
            .with(self.token, f)
    }
}
