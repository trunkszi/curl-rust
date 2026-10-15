//! Multi-request HTTP handles sharing one event loop.
use crate::easy::{Easy, List};
use crate::event_loop::{Error, EventLoop, LoopInner, Token};
use std::marker::PhantomData;
use std::net::IpAddr;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

fn next_client_id() -> u64 {
    NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Default, Clone)]
struct Defaults {
    resolves: Vec<(String, u16, Vec<IpAddr>)>,
    invalid_resolve: bool,
    cainfo: Option<String>,
    proxy: Option<String>,
    timeout: Option<Duration>,
    /// Pre-normalized `(lowercase_trimmed_name, formatted_header_line)` entries.
    headers: Vec<(String, String)>,
}

impl Defaults {
    fn add_resolve(&mut self, host: &str, port: u16, addrs: &[IpAddr]) -> Result<(), crate::Error> {
        let normalized = Easy::normalize_resolve_host(host).to_ascii_lowercase();
        if normalized.is_empty() || addrs.is_empty() {
            return Err(crate::Error::new(curl_sys::CURLE_BAD_FUNCTION_ARGUMENT));
        }
        let mut deduped = Vec::with_capacity(addrs.len());
        for &addr in addrs {
            if !deduped.contains(&addr) {
                deduped.push(addr);
            }
        }
        if let Some((_, _, existing)) = self
            .resolves
            .iter_mut()
            .find(|(h, p, _)| h == &normalized && *p == port)
        {
            *existing = deduped;
        } else {
            self.resolves.push((normalized, port, deduped));
        }
        Ok(())
    }

    fn add_header(&mut self, name: &str, value: &str) {
        let key = name.trim().to_ascii_lowercase();
        let line = format!("{}: {}", name.trim(), value);
        if let Some((_, existing_line)) = self.headers.iter_mut().find(|(k, _)| k == &key) {
            *existing_line = line;
        } else {
            self.headers.push((key, line));
        }
    }
}

/// Builder for [`HttpClient`] that pre-validates and pre-merges default headers and DNS entries.
#[derive(Debug)]
pub struct HttpClientBuilder {
    loop_: Weak<LoopInner>,
    defaults: Defaults,
    _marker: PhantomData<*mut ()>,
}

impl HttpClientBuilder {
    /// Creates a new builder bound weakly to `loop_`.
    pub fn new(loop_: &EventLoop) -> Self {
        Self {
            loop_: Rc::downgrade(&loop_.inner),
            defaults: Defaults::default(),
            _marker: PhantomData,
        }
    }

    /// Preloads a host-to-IP mapping into the client defaults.
    pub fn resolve_host(
        &mut self,
        host: &str,
        port: u16,
        addrs: &[IpAddr],
    ) -> Result<&mut Self, crate::Error> {
        self.defaults.add_resolve(host, port, addrs)?;
        Ok(self)
    }

    /// Sets the default CA bundle path.
    pub fn cainfo(&mut self, path: &str) -> &mut Self {
        self.defaults.cainfo = Some(path.into());
        self
    }

    /// Sets the default proxy URL.
    pub fn proxy(&mut self, url: &str) -> &mut Self {
        self.defaults.proxy = Some(url.into());
        self
    }

    /// Sets the default transfer timeout.
    pub fn timeout(&mut self, d: Duration) -> &mut Self {
        self.defaults.timeout = Some(d);
        self
    }

    /// Adds or replaces a default HTTP header.
    pub fn header(&mut self, name: &str, value: &str) -> &mut Self {
        self.defaults.add_header(name, value);
        self
    }

    /// Builds an [`HttpClient`] with pre-merged defaults.
    pub fn build(&self) -> HttpClient {
        HttpClient {
            id: next_client_id(),
            loop_: self.loop_.clone(),
            defaults: self.defaults.clone(),
            _marker: PhantomData,
        }
    }
}

/// HTTP client configuration bound weakly to an event loop.
#[derive(Debug)]
pub struct HttpClient {
    id: u64,
    loop_: Weak<LoopInner>,
    defaults: Defaults,
    _marker: PhantomData<*mut ()>,
}

impl HttpClient {
    /// Creates a client sharing `loop_`.
    pub fn new(loop_: &EventLoop) -> Self {
        Self {
            id: next_client_id(),
            loop_: Rc::downgrade(&loop_.inner),
            defaults: Defaults::default(),
            _marker: PhantomData,
        }
    }

    /// Creates a builder sharing `loop_`.
    pub fn builder(loop_: &EventLoop) -> HttpClientBuilder {
        HttpClientBuilder::new(loop_)
    }

    /// Public interface for this event-loop operation.
    pub fn resolve_host(mut self, host: &str, port: u16, addrs: &[IpAddr]) -> Self {
        if self.defaults.add_resolve(host, port, addrs).is_err() {
            self.defaults.invalid_resolve = true;
        }
        self.id = next_client_id();
        self
    }

    /// Public interface for this event-loop operation.
    pub fn cainfo(mut self, path: &str) -> Self {
        self.defaults.cainfo = Some(path.into());
        self.id = next_client_id();
        self
    }

    /// Public interface for this event-loop operation.
    pub fn proxy(mut self, url: &str) -> Self {
        self.defaults.proxy = Some(url.into());
        self.id = next_client_id();
        self
    }

    /// Public interface for this event-loop operation.
    pub fn timeout(mut self, d: Duration) -> Self {
        self.defaults.timeout = Some(d);
        self.id = next_client_id();
        self
    }

    /// Public interface for this event-loop operation.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.defaults.add_header(name, value);
        self.id = next_client_id();
        self
    }

    /// Submits a transfer with this client's defaults.
    ///
    /// Explicit request overrides are consumed only on successful admission.
    /// On rejection, the returned `AddError::easy` retains them for retry.
    pub fn send<F>(
        &self,
        mut easy: Easy,
        on_done: F,
    ) -> Result<RequestHandle, crate::event_loop::AddError>
    where
        F: FnOnce(Easy, Result<(), Error>) + 'static,
    {
        let loop_ = match self.loop_.upgrade() {
            Some(loop_) => loop_,
            None => {
                return Err(crate::event_loop::AddError {
                    easy,
                    error: Error::LoopGone,
                });
            }
        };
        // Defaults can mutate the handle before admission fails. Only an
        // accepted request consumes its explicit overrides.
        let restore_overrides = {
            let explicit_resolve = easy.inner.explicit_resolve;
            let resolved_hosts_from_client = easy.inner.resolved_hosts_from_client;
            let explicit_headers = easy.inner.explicit_headers;
            let explicit_cainfo = easy.inner.explicit_cainfo;
            let explicit_proxy = easy.inner.explicit_proxy;
            let explicit_timeout = easy.inner.explicit_timeout;
            move |easy: &mut Easy| {
                easy.inner.explicit_resolve = explicit_resolve;
                easy.inner.resolved_hosts_from_client = resolved_hosts_from_client;
                easy.inner.explicit_headers = explicit_headers;
                easy.inner.explicit_cainfo = explicit_cainfo;
                easy.inner.explicit_proxy = explicit_proxy;
                easy.inner.explicit_timeout = explicit_timeout;
                easy.inner.client_config_id = None;
            }
        };
        macro_rules! apply {
            ($call:expr) => {
                if let Err(error) = $call {
                    restore_overrides(&mut easy);
                    return Err(crate::event_loop::AddError {
                        easy,
                        error: Error::Curl(error),
                    });
                }
            };
        }

        if self.defaults.invalid_resolve {
            return Err(crate::event_loop::AddError {
                easy,
                error: Error::Curl(crate::Error::new(curl_sys::CURLE_BAD_FUNCTION_ARGUMENT)),
            });
        }

        let is_fast_recycled = easy.inner.client_config_id == Some(self.id)
            && !easy.inner.explicit_resolve
            && !easy.inner.explicit_headers
            && !easy.inner.explicit_cainfo
            && !easy.inner.explicit_proxy
            && !easy.inner.explicit_timeout;

        if is_fast_recycled {
            apply!(easy.rearm_resolve());
        } else {
            let had_explicit_resolve = easy.inner.explicit_resolve;
            let had_explicit_cainfo = easy.inner.explicit_cainfo;
            let had_explicit_proxy = easy.inner.explicit_proxy;
            let had_explicit_timeout = easy.inner.explicit_timeout;
            let had_explicit_headers = easy.inner.explicit_headers;

            apply!(easy.apply_client_resolves(&self.defaults.resolves, had_explicit_resolve));

            if !had_explicit_cainfo {
                if let Some(path) = &self.defaults.cainfo {
                    apply!(easy.cainfo(path));
                }
            }
            easy.inner.explicit_cainfo = false;

            if !had_explicit_proxy {
                if let Some(proxy) = &self.defaults.proxy {
                    apply!(easy.proxy(proxy));
                } else if easy.inner.proxy_url.is_some() {
                    apply!(easy.proxy(""));
                    easy.inner.proxy_url = None;
                }
                if easy.inner.proxy_port.is_some() {
                    let _ = easy.proxy_port(0);
                    easy.inner.proxy_port = None;
                }
            }
            easy.inner.explicit_proxy = false;

            if !had_explicit_timeout {
                if let Some(timeout) = self.defaults.timeout {
                    apply!(easy.timeout(timeout));
                } else {
                    apply!(easy.clear_timeout());
                }
            }
            easy.inner.explicit_timeout = false;

            if had_explicit_headers {
                if !self.defaults.headers.is_empty() {
                    let mut list = List::new();
                    let mut names =
                        std::collections::HashSet::with_capacity(self.defaults.headers.len() + 4);
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
                    for (key, line) in &self.defaults.headers {
                        if !names.contains(key) {
                            apply!(list.append(line));
                        }
                    }
                    apply!(easy.http_headers(list));
                }
            } else if !self.defaults.headers.is_empty() {
                let mut list = List::new();
                for (_, line) in &self.defaults.headers {
                    apply!(list.append(line));
                }
                apply!(easy.http_headers(list));
            } else if easy.inner.header_list.is_some() {
                apply!(easy.clear_headers());
            }
            easy.inner.explicit_headers = false;

            if !had_explicit_resolve
                && !had_explicit_cainfo
                && !had_explicit_proxy
                && !had_explicit_timeout
                && !had_explicit_headers
            {
                easy.inner.client_config_id = Some(self.id);
            } else {
                easy.inner.client_config_id = None;
            }
        }

        let token = loop_
            .enqueue(easy, Some(Box::new(on_done)))
            .map_err(|mut error| {
                restore_overrides(&mut error.easy);
                error
            })?;
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

