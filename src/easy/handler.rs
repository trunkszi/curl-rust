use std::cell::RefCell;
use std::convert::TryFrom;
use std::ffi::{CStr, CString};
use std::fmt;
use std::io::{self, SeekFrom, Write};
use std::marker::PhantomData;
use std::net::IpAddr;
use std::path::Path;
use std::ptr;
use std::slice;
use std::str;
use std::time::Duration;

use libc::{c_char, c_double, c_int, c_long, c_ulong, c_void, size_t};
use socket2::Socket;

use crate::Error;
use crate::easy::form;
use crate::easy::list;
use crate::easy::{Form, List};
use crate::panic;

pub(super) fn open_socket(
    family: c_int,
    socktype: c_int,
    protocol: c_int,
) -> Option<curl_sys::curl_socket_t> {
    use std::os::fd::IntoRawFd;
    Socket::new(family.into(), socktype.into(), Some(protocol.into()))
        .ok()
        .map(IntoRawFd::into_raw_fd)
}

pub fn debug(kind: InfoType, data: &[u8]) {
    let out = io::stderr();
    let prefix = match kind {
        InfoType::Text => "*",
        InfoType::HeaderIn => "<",
        InfoType::HeaderOut => ">",
        InfoType::DataIn | InfoType::SslDataIn => "{",
        InfoType::DataOut | InfoType::SslDataOut => "}",
    };
    let mut out = out.lock();
    drop(write!(out, "{} ", prefix));
    match str::from_utf8(data) {
        Ok(s) => drop(out.write_all(s.as_bytes())),
        Err(_) => drop(writeln!(out, "({} bytes of data)", data.len())),
    }
}

/// A single-thread libcurl session configured with owned closures.
///
/// Callbacks may capture `Rc<RefCell<T>>` for shared state. Handles and their
/// callbacks remain on the creating thread.
///
/// This value cannot move to another thread:
/// ```compile_fail
/// let value = curl::easy::Easy::new();
/// std::thread::spawn(move || drop(value));
/// ```
/// It cannot be shared across threads through `Arc`:
/// ```compile_fail
/// let value = std::sync::Arc::new(curl::easy::Easy::new());
/// std::thread::spawn(move || drop(value));
/// ```
pub struct Easy {
    pub(crate) inner: Box<Inner>,
    _marker: PhantomData<*mut ()>,
}

pub(crate) struct Inner {
    handle: *mut curl_sys::CURL,
    pub(crate) header_list: Option<List>,
    pub(crate) resolve_list: Option<List>,
    proxy_header_list: Option<List>,
    form: Option<Form>,
    error_buf: RefCell<Vec<u8>>,
    callbacks: Callbacks,
    pub(crate) configured_url: Option<String>,
    pub(crate) configured_port: Option<u16>,
    pub(crate) proxy_url: Option<String>,
    pub(crate) proxy_port: Option<u16>,
    pub(crate) resolved_hosts: Vec<(String, u16, Vec<IpAddr>)>,
    ssl_options: c_long,
    pub(crate) explicit_resolve: bool,
    pub(crate) resolved_hosts_from_client: bool,
    pub(crate) explicit_headers: bool,
    pub(crate) explicit_proxy: bool,
    pub(crate) explicit_timeout: bool,
    pub(crate) explicit_cainfo: bool,
    pub(crate) client_config_id: Option<u64>,
}

/// Possible proxy types that libcurl currently understands.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum ProxyType {
    /// Selects the Http option.
    Http = curl_sys::CURLPROXY_HTTP as isize,
    /// Selects the Http1 option.
    Http1 = curl_sys::CURLPROXY_HTTP_1_0 as isize,
    /// Selects the Socks4 option.
    Socks4 = curl_sys::CURLPROXY_SOCKS4 as isize,
    /// Selects the Socks5 option.
    Socks5 = curl_sys::CURLPROXY_SOCKS5 as isize,
    /// Selects the Socks4a option.
    Socks4a = curl_sys::CURLPROXY_SOCKS4A as isize,
    /// Selects the Socks5Hostname option.
    Socks5Hostname = curl_sys::CURLPROXY_SOCKS5_HOSTNAME as isize,
}

/// Possible conditions for the `time_condition` method.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum TimeCondition {
    /// Selects the None option.
    None = curl_sys::CURL_TIMECOND_NONE as isize,
    /// Selects the IfModifiedSince option.
    IfModifiedSince = curl_sys::CURL_TIMECOND_IFMODSINCE as isize,
    /// Selects the IfUnmodifiedSince option.
    IfUnmodifiedSince = curl_sys::CURL_TIMECOND_IFUNMODSINCE as isize,
    /// Selects the LastModified option.
    LastModified = curl_sys::CURL_TIMECOND_LASTMOD as isize,
}

/// Possible values to pass to the `ip_resolve` method.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum IpResolve {
    /// Selects the V4 option.
    V4 = curl_sys::CURL_IPRESOLVE_V4 as isize,
    /// Selects the V6 option.
    V6 = curl_sys::CURL_IPRESOLVE_V6 as isize,
    /// Selects the Any option.
    Any = curl_sys::CURL_IPRESOLVE_WHATEVER as isize,
}

/// Possible values to pass to the `http_version` method.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum HttpVersion {
    /// We don't care what http version to use, and we'd like the library to
    /// choose the best possible for us.
    Any = curl_sys::CURL_HTTP_VERSION_NONE as isize,

    /// Please use HTTP 1.0 in the request
    V10 = curl_sys::CURL_HTTP_VERSION_1_0 as isize,

    /// Please use HTTP 1.1 in the request
    V11 = curl_sys::CURL_HTTP_VERSION_1_1 as isize,

    /// Please use HTTP 2 in the request
    /// (Added in CURL 7.33.0)
    V2 = curl_sys::CURL_HTTP_VERSION_2_0 as isize,

    /// Use version 2 for HTTPS, version 1.1 for HTTP
    /// (Added in CURL 7.47.0)
    V2TLS = curl_sys::CURL_HTTP_VERSION_2TLS as isize,

    /// Please use HTTP 2 without HTTP/1.1 Upgrade
    /// (Added in CURL 7.49.0)
    V2PriorKnowledge = curl_sys::CURL_HTTP_VERSION_2_PRIOR_KNOWLEDGE as isize,

    /// Setting this value will make libcurl attempt to use HTTP/3 directly to
    /// server given in the URL but fallback to earlier HTTP versions if the HTTP/3
    /// connection establishment fails.
    ///
    /// Note: the meaning of this settings depends on the linked libcurl.
    /// For CURL < 7.88.0, there is no fallback if HTTP/3 connection fails.
    ///
    /// (Added in CURL 7.66.0)
    V3 = curl_sys::CURL_HTTP_VERSION_3 as isize,
}

/// Possible values to pass to the `ssl_version` and `ssl_min_max_version` method.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum SslVersion {
    /// Selects the Default option.
    Default = curl_sys::CURL_SSLVERSION_DEFAULT as isize,
    /// Selects the Tlsv1 option.
    Tlsv1 = curl_sys::CURL_SSLVERSION_TLSv1 as isize,
    /// Selects the Sslv2 option.
    Sslv2 = curl_sys::CURL_SSLVERSION_SSLv2 as isize,
    /// Selects the Sslv3 option.
    Sslv3 = curl_sys::CURL_SSLVERSION_SSLv3 as isize,
    /// Selects the Tlsv10 option.
    Tlsv10 = curl_sys::CURL_SSLVERSION_TLSv1_0 as isize,
    /// Selects the Tlsv11 option.
    Tlsv11 = curl_sys::CURL_SSLVERSION_TLSv1_1 as isize,
    /// Selects the Tlsv12 option.
    Tlsv12 = curl_sys::CURL_SSLVERSION_TLSv1_2 as isize,
    /// Selects the Tlsv13 option.
    Tlsv13 = curl_sys::CURL_SSLVERSION_TLSv1_3 as isize,
}

/// Possible return values from the `seek_function` callback.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum SeekResult {
    /// Indicates that the seek operation was a success
    Ok = curl_sys::CURL_SEEKFUNC_OK as isize,

    /// Indicates that the seek operation failed, and the entire request should
    /// fail as a result.
    Fail = curl_sys::CURL_SEEKFUNC_FAIL as isize,

    /// Indicates that although the seek failed libcurl should attempt to keep
    /// working if possible (for example "seek" through reading).
    CantSeek = curl_sys::CURL_SEEKFUNC_CANTSEEK as isize,
}

/// Possible data chunks that can be witnessed as part of the `debug_function`
/// callback.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum InfoType {
    /// The data is informational text.
    Text,

    /// The data is header (or header-like) data received from the peer.
    HeaderIn,

    /// The data is header (or header-like) data sent to the peer.
    HeaderOut,

    /// The data is protocol data received from the peer.
    DataIn,

    /// The data is protocol data sent to the peer.
    DataOut,

    /// The data is SSL/TLS (binary) data received from the peer.
    SslDataIn,

    /// The data is SSL/TLS (binary) data sent to the peer.
    SslDataOut,
}

/// Possible error codes that can be returned from the `read_function` callback.
#[non_exhaustive]
#[derive(Debug)]
pub enum ReadError {
    /// Indicates that the connection should be aborted immediately
    Abort,

    /// Indicates that reading should be paused until `unpause` is called.
    Pause,
}

/// Possible error codes that can be returned from the `write_function` callback.
#[non_exhaustive]
#[derive(Debug)]
pub enum WriteError {
    /// Indicates that reading should be paused until `unpause` is called.
    Pause,
}

/// Structure which stores possible authentication methods to get passed to
/// `http_auth` and `proxy_auth`.
#[derive(Clone)]
pub struct Auth {
    bits: c_long,
}

/// Structure which stores possible ssl options to pass to `ssl_options`.
#[derive(Clone)]
pub struct SslOpt {
    bits: c_long,
}

impl Easy {
    pub(crate) fn active_socket(&self) -> Option<i32> {
        let mut fd: libc::c_int = -1;
        let rc = unsafe {
            curl_sys::curl_easy_getinfo(self.inner.handle, curl_sys::CURLINFO_ACTIVESOCKET, &mut fd)
        };
        (rc == curl_sys::CURLE_OK && fd >= 0).then_some(fd)
    }
    /// Validate every host libcurl could connect to without performing DNS.
    ///
    /// This is crate-visible so the event-loop admission seam can enforce the
    /// application's DNS ownership before handing the handle to libcurl.
    pub(crate) fn validate_targets(&self) -> bool {
        fn is_valid_scheme(s: &str) -> bool {
            let mut bytes = s.bytes();
            matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z'))
                && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        }

        fn authority(
            url: &str,
            fallback_port: u16,
            forced_port: Option<u16>,
        ) -> Option<(String, u16)> {
            let (scheme, rest) = url.split_once("://")?;
            if !is_valid_scheme(scheme) {
                return None;
            }
            let authority = rest.split(['/', '?', '#']).next()?;
            if authority.is_empty() {
                return None;
            }
            let authority = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
                let end = bracketed.find(']')?;
                let host = &bracketed[..end];
                let host_for_parse = host.split_once("%25").map_or(host, |(h, _)| h);
                host_for_parse.parse::<IpAddr>().ok()?;
                let suffix = &bracketed[end + 1..];
                let port = suffix
                    .strip_prefix(':')
                    .filter(|p| !p.is_empty())
                    .map_or(Ok(fallback_port), str::parse)
                    .ok()?;
                (host.to_ascii_lowercase(), forced_port.unwrap_or(port))
            } else {
                let (host, port) = match authority
                    .rsplit_once(':')
                    .filter(|(_, p)| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                {
                    Some((h, p)) => (h, p.parse().ok()?),
                    None => (authority, fallback_port),
                };
                if host.parse::<IpAddr>().is_err() {
                    if host.is_empty() || host.contains(':') {
                        return None;
                    }
                }
                (host.to_ascii_lowercase(), forced_port.unwrap_or(port))
            };
            Some((host, port))
        }

        fn prepared(
            target: Option<(String, u16)>,
            mappings: &[(String, u16, Vec<IpAddr>)],
        ) -> bool {
            let Some((host, port)) = target else {
                return false;
            };
            let ip_literal = host
                .split_once("%25")
                .map_or(host.as_str(), |(ip, _)| ip)
                .parse::<IpAddr>()
                .is_ok();
            ip_literal
                || mappings
                    .iter()
                    .any(|(mapped, mapped_port, _)| mapped == &host && mapped_port == &port)
        }

        let Some(url) = self.inner.configured_url.as_deref() else {
            return false;
        };
        let scheme = url
            .split_once("://")
            .filter(|(s, _)| is_valid_scheme(s))
            .map_or("", |(s, _)| s);
        let url_port = self.inner.configured_port.unwrap_or(
            if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("wss") {
                443
            } else {
                80
            },
        );
        if !prepared(
            authority(url, url_port, self.inner.configured_port),
            &self.inner.resolved_hosts,
        ) {
            return false;
        }
        let Some(proxy) = self.inner.proxy_url.as_deref().filter(|p| !p.is_empty()) else {
            return true;
        };
        let (proxy_scheme, proxy_url) = match proxy.split_once("://") {
            Some((s, _)) if is_valid_scheme(s) => (s, proxy.to_owned()),
            _ => ("", format!("proxy://{proxy}")),
        };
        let proxy_port =
            self.inner
                .proxy_port
                .unwrap_or(if proxy_scheme.eq_ignore_ascii_case("https") {
                    443
                } else {
                    1080
                });
        prepared(
            // Unlike CURLOPT_PORT, CURLOPT_PROXYPORT is only a fallback when
            // the proxy URL has no explicit port.
            authority(&proxy_url, proxy_port, None),
            &self.inner.resolved_hosts,
        )
    }

    pub(crate) fn normalize_resolve_host(host: &str) -> &str {
        host.strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(host)
    }

    pub(crate) fn format_resolve_entry(host: &str, port: u16, addrs: &[IpAddr]) -> String {
        let joined = addrs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        if host.contains(':') {
            format!("[{host}]:{port}:{joined}")
        } else {
            format!("{host}:{port}:{joined}")
        }
    }

    /// Preinstall application-resolved addresses for a host and port.
    pub fn resolve_host(&mut self, host: &str, port: u16, addrs: &[IpAddr]) -> Result<(), Error> {
        let stripped = Self::normalize_resolve_host(host);
        if stripped.is_empty() || addrs.is_empty() {
            return Err(Error::new(curl_sys::CURLE_BAD_FUNCTION_ARGUMENT));
        }
        if self.inner.resolved_hosts_from_client {
            self.inner.resolved_hosts.clear();
            self.inner.resolve_list = None;
            self.inner.resolved_hosts_from_client = false;
        }
        self.inner.explicit_resolve = true;
        self.inner.client_config_id = None;
        self.add_resolve_internal(stripped, port, addrs)
    }

    pub(crate) fn add_resolve_internal(
        &mut self,
        stripped_host: &str,
        port: u16,
        addrs: &[IpAddr],
    ) -> Result<(), Error> {
        let mut dedup_addrs = Vec::with_capacity(addrs.len());
        for &addr in addrs {
            if !dedup_addrs.contains(&addr) {
                dedup_addrs.push(addr);
            }
        }
        let normalized = stripped_host.to_ascii_lowercase();
        if let Some(pos) = self
            .inner
            .resolved_hosts
            .iter()
            .position(|(h, p, _)| h == &normalized && *p == port)
        {
            if self.inner.resolved_hosts[pos].2 == dedup_addrs {
                if let Some(ref list) = self.inner.resolve_list {
                    let ptr = list::raw(list);
                    self.setopt_ptr(curl_sys::CURLOPT_RESOLVE, ptr as *const _)?;
                }
                return Ok(());
            }
            let mut list = List::new();
            for (idx, (h, p, existing_addrs)) in self.inner.resolved_hosts.iter().enumerate() {
                let ips = if idx == pos {
                    &dedup_addrs
                } else {
                    existing_addrs
                };
                list.append(&Self::format_resolve_entry(h, *p, ips))?;
            }
            self.resolve(list)?;
            self.inner.resolved_hosts[pos].2 = dedup_addrs;
            return Ok(());
        }

        let entry = Self::format_resolve_entry(&normalized, port, &dedup_addrs);
        let mut list = self.inner.resolve_list.take().unwrap_or_else(List::new);
        if let Err(err) = list.append(&entry) {
            self.inner.resolve_list = Some(list);
            return Err(err);
        }
        self.resolve(list)?;
        self.inner
            .resolved_hosts
            .push((normalized, port, dedup_addrs));
        Ok(())
    }

    pub(crate) fn apply_client_resolves(
        &mut self,
        defaults: &[(String, u16, Vec<IpAddr>)],
        had_explicit_resolve: bool,
    ) -> Result<(), Error> {
        if !had_explicit_resolve {
            self.inner.resolved_hosts.clear();
            self.inner.resolve_list = None;
            if defaults.is_empty() {
                self.setopt_ptr(curl_sys::CURLOPT_RESOLVE, std::ptr::null())?;
            } else {
                let mut list = List::new();
                for (h, p, ips) in defaults {
                    list.append(&Self::format_resolve_entry(h, *p, ips))?;
                    self.inner.resolved_hosts.push((h.clone(), *p, ips.clone()));
                }
                self.resolve(list)?;
            }
        } else {
            for (h, p, ips) in defaults {
                if !self
                    .inner
                    .resolved_hosts
                    .iter()
                    .any(|(eh, ep, _)| eh == h && ep == p)
                {
                    self.add_resolve_internal(h, *p, ips)?;
                }
            }
        }
        self.inner.resolved_hosts_from_client = true;
        self.inner.explicit_resolve = false;
        Ok(())
    }

    pub(crate) fn clear_timeout(&mut self) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_TIMEOUT_MS, 0)
    }

    pub(crate) fn clear_headers(&mut self) -> Result<(), Error> {
        self.inner.header_list = None;
        self.setopt_ptr(curl_sys::CURLOPT_HTTPHEADER, std::ptr::null())
    }

    /// Creates a new "easy" handle which is the core of almost all operations
    /// in libcurl.
    ///
    /// To use a handle, applications typically configure a number of options
    /// followed by submitting the handle to an [`EventLoop`][crate::EventLoop].
    /// Options are preserved across transfers and need to be reset manually
    /// (or via the `reset` method) if this is not desired.
    pub fn new() -> Easy {
        crate::init();
        unsafe {
            let handle = curl_sys::curl_easy_init();
            assert!(!handle.is_null());
            let mut ret = Easy {
                _marker: PhantomData,
                inner: Box::new(Inner {
                    handle,
                    header_list: None,
                    resolve_list: None,
                    proxy_header_list: None,
                    form: None,
                    error_buf: RefCell::new(vec![0; curl_sys::CURL_ERROR_SIZE]),
                    callbacks: Callbacks::default(),
                    configured_url: None,
                    configured_port: None,
                    proxy_url: None,
                    proxy_port: None,
                    resolved_hosts: Vec::new(),
                    ssl_options: 0,
                    explicit_resolve: false,
                    resolved_hosts_from_client: false,
                    explicit_headers: false,
                    explicit_proxy: false,
                    explicit_timeout: false,
                    explicit_cainfo: false,
                    client_config_id: None,
                }),
            };
            ret.default_configure();
            ret
        }
    }

    /// Re-initializes this handle to the default values.
    ///
    /// This puts the handle to the same state as it was in when it was just
    /// created. This does, however, keep live connections, the session id
    /// cache, the dns cache, and cookies.
    pub fn reset(&mut self) {
        self.inner.configured_url = None;
        self.inner.configured_port = None;
        self.inner.proxy_url = None;
        self.inner.proxy_port = None;
        self.inner.resolved_hosts.clear();
        self.inner.explicit_resolve = false;
        self.inner.resolved_hosts_from_client = false;
        self.inner.explicit_headers = false;
        self.inner.explicit_proxy = false;
        self.inner.explicit_timeout = false;
        self.inner.explicit_cainfo = false;
        self.inner.client_config_id = None;
        self.inner.header_list = None;
        self.inner.resolve_list = None;
        unsafe {
            curl_sys::curl_easy_reset(self.inner.handle);
        }
        self.default_configure();
    }

    fn default_configure(&mut self) {
        self.setopt_ptr(
            curl_sys::CURLOPT_ERRORBUFFER,
            self.inner.error_buf.borrow().as_ptr() as *const _,
        )
        .expect("failed to set error buffer");
        let _ = self.signal(false);
        let _ = self.tcp_nodelay(true);
        self.inner.ssl_options = curl_sys::CURLSSLOPT_NATIVE_CA as c_long;
        let _ = self.setopt_long(curl_sys::CURLOPT_SSL_OPTIONS, self.inner.ssl_options);

        let ptr = &*self.inner as *const _ as *const _;

        let cb: extern "C" fn(*mut c_char, size_t, size_t, *mut c_void) -> size_t = header_cb;
        self.setopt_ptr(curl_sys::CURLOPT_HEADERFUNCTION, cb as *const _)
            .expect("failed to set header callback");
        self.setopt_ptr(curl_sys::CURLOPT_HEADERDATA, ptr)
            .expect("failed to set header callback");

        let cb: curl_sys::curl_write_callback = write_cb;
        self.setopt_ptr(curl_sys::CURLOPT_WRITEFUNCTION, cb as *const _)
            .expect("failed to set write callback");
        self.setopt_ptr(curl_sys::CURLOPT_WRITEDATA, ptr)
            .expect("failed to set write callback");

        let cb: curl_sys::curl_read_callback = read_cb;
        self.setopt_ptr(curl_sys::CURLOPT_READFUNCTION, cb as *const _)
            .expect("failed to set read callback");
        self.setopt_ptr(curl_sys::CURLOPT_READDATA, ptr)
            .expect("failed to set read callback");

        let cb: curl_sys::curl_seek_callback = seek_cb;
        self.setopt_ptr(curl_sys::CURLOPT_SEEKFUNCTION, cb as *const _)
            .expect("failed to set seek callback");
        self.setopt_ptr(curl_sys::CURLOPT_SEEKDATA, ptr)
            .expect("failed to set seek callback");

        let cb: curl_sys::curl_progress_callback = progress_cb;
        self.setopt_ptr(curl_sys::CURLOPT_PROGRESSFUNCTION, cb as *const _)
            .expect("failed to set progress callback");
        self.setopt_ptr(curl_sys::CURLOPT_PROGRESSDATA, ptr)
            .expect("failed to set progress callback");

        let cb: curl_sys::curl_debug_callback = debug_cb;
        self.setopt_ptr(curl_sys::CURLOPT_DEBUGFUNCTION, cb as *const _)
            .expect("failed to set debug callback");
        self.setopt_ptr(curl_sys::CURLOPT_DEBUGDATA, ptr)
            .expect("failed to set debug callback");

        let cb: curl_sys::curl_opensocket_callback = opensocket_cb;
        self.setopt_ptr(curl_sys::CURLOPT_OPENSOCKETFUNCTION, cb as *const _)
            .expect("failed to set open socket callback");
        self.setopt_ptr(curl_sys::CURLOPT_OPENSOCKETDATA, ptr)
            .expect("failed to set open socket callback");
    }
}

impl Easy {
    // =========================================================================
    // Behavior options

    /// Configures this handle to have verbose output to help debug protocol
    /// information.
    ///
    /// By default output goes to stderr, but the `stderr` function on this type
    /// can configure that. You can also use the `debug_function` method to get
    /// all protocol data sent and received.
    ///
    /// By default, this option is `false`.
    pub fn verbose(&mut self, verbose: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_VERBOSE, verbose as c_long)
    }

    /// Indicates whether header information is streamed to the output body of
    /// this request.
    ///
    /// This option is only relevant for protocols which have header metadata
    /// (like http or ftp). It's not generally possible to extract headers
    /// from the body if using this method, that use case should be intended for
    /// the `header_function` method.
    ///
    /// To set HTTP headers, use the `http_header` method.
    ///
    /// By default, this option is `false` and corresponds to
    /// `CURLOPT_HEADER`.
    pub fn show_header(&mut self, show: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_HEADER, show as c_long)
    }

    /// Indicates whether a progress meter will be shown for requests done with
    /// this handle.
    ///
    /// This will also prevent the `progress_function` from being called.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_NOPROGRESS`.
    pub fn progress(&mut self, progress: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_NOPROGRESS, (!progress) as c_long)
    }

    /// Inform libcurl whether or not it should install signal handlers or
    /// attempt to use signals to perform library functions.
    ///
    /// If this option is disabled then timeouts during name resolution will not
    /// work unless libcurl is built against c-ares. Note that enabling this
    /// option, however, may not cause libcurl to work with multiple threads.
    ///
    /// By default this option is `false` and corresponds to `CURLOPT_NOSIGNAL`.
    /// Note that this default is **different than libcurl** as it is intended
    /// that this library is threadsafe by default. See the [libcurl docs] for
    /// some more information.
    ///
    /// [libcurl docs]: https://curl.haxx.se/libcurl/c/threadsafe.html
    pub fn signal(&mut self, signal: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_NOSIGNAL, (!signal) as c_long)
    }

    /// Indicates whether multiple files will be transferred based on the file
    /// name pattern.
    ///
    /// The last part of a filename uses fnmatch-like pattern matching.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_WILDCARDMATCH`.
    pub fn wildcard_match(&mut self, m: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_WILDCARDMATCH, m as c_long)
    }

    /// Provides the Unix domain socket which this handle will work with.
    ///
    /// The string provided must be a path to a Unix domain socket encoded with
    /// the format:
    ///
    /// ```text
    /// /path/file.sock
    /// ```
    ///
    /// By default this option is not set and corresponds to
    /// [`CURLOPT_UNIX_SOCKET_PATH`](https://curl.haxx.se/libcurl/c/CURLOPT_UNIX_SOCKET_PATH.html).
    pub fn unix_socket(&mut self, unix_domain_socket: &str) -> Result<(), Error> {
        let socket = CString::new(unix_domain_socket)?;
        self.setopt_str(curl_sys::CURLOPT_UNIX_SOCKET_PATH, &socket)
    }

    /// Provides the Unix domain socket which this handle will work with.
    ///
    /// The string provided must be a path to a Unix domain socket encoded with
    /// the format:
    ///
    /// ```text
    /// /path/file.sock
    /// ```
    ///
    /// This function is an alternative to [`Easy::unix_socket`] that supports
    /// non-UTF-8 paths and also supports disabling Unix sockets by setting the
    /// option to `None`.
    ///
    /// By default this option is not set and corresponds to
    /// [`CURLOPT_UNIX_SOCKET_PATH`](https://curl.haxx.se/libcurl/c/CURLOPT_UNIX_SOCKET_PATH.html).
    pub fn unix_socket_path<P: AsRef<Path>>(&mut self, path: Option<P>) -> Result<(), Error> {
        if let Some(path) = path {
            self.setopt_path(curl_sys::CURLOPT_UNIX_SOCKET_PATH, path.as_ref())
        } else {
            self.setopt_ptr(curl_sys::CURLOPT_UNIX_SOCKET_PATH, 0 as _)
        }
    }

    /// Provides the ABSTRACT UNIX SOCKET which this handle will work with.
    ///
    /// This function is an alternative to [`Easy::unix_socket`] and [`Easy::unix_socket_path`] that supports
    /// ABSTRACT_UNIX_SOCKET(`man 7 unix` on Linux) address.
    ///
    /// By default this option is not set and corresponds to
    /// [`CURLOPT_ABSTRACT_UNIX_SOCKET`](https://curl.haxx.se/libcurl/c/CURLOPT_ABSTRACT_UNIX_SOCKET.html).
    ///
    /// NOTE: this API can only be used on Linux OS.
    #[cfg(target_os = "linux")]
    pub fn abstract_unix_socket(&mut self, addr: &[u8]) -> Result<(), Error> {
        let addr = CString::new(addr)?;
        self.setopt_str(curl_sys::CURLOPT_ABSTRACT_UNIX_SOCKET, &addr)
    }

    // =========================================================================
    // Internal accessors

    // =========================================================================
    // Error options

    // TODO: error buffer and stderr

    /// Indicates whether this library will fail on HTTP response codes >= 400.
    ///
    /// This method is not fail-safe especially when authentication is involved.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_FAILONERROR`.
    pub fn fail_on_error(&mut self, fail: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_FAILONERROR, fail as c_long)
    }

    // =========================================================================
    // Network options

    /// Provides the URL which this handle will work with.
    ///
    /// The string provided must be URL-encoded with the format:
    ///
    /// ```text
    /// scheme://host:port/path
    /// ```
    ///
    /// The syntax is not validated as part of this function and that is
    /// deferred until later.
    ///
    /// By default this option is not set and `perform` will not work until it
    /// is set. This option corresponds to `CURLOPT_URL`.
    pub fn url(&mut self, url: &str) -> Result<(), Error> {
        self.inner.configured_url = Some(url.to_string());
        let url = CString::new(url)?;
        self.setopt_str(curl_sys::CURLOPT_URL, &url)
    }

    /// Configures the port number to connect to, instead of the one specified
    /// in the URL or the default of the protocol.
    pub fn port(&mut self, port: u16) -> Result<(), Error> {
        self.inner.configured_port = Some(port);
        self.setopt_long(curl_sys::CURLOPT_PORT, port as c_long)
    }

    /// Indicates whether sequences of `/../` and `/./` will be squashed or not.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_PATH_AS_IS`.
    pub fn path_as_is(&mut self, as_is: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_PATH_AS_IS, as_is as c_long)
    }

    /// Provide the URL of a proxy to use.
    ///
    /// By default this option is not set and corresponds to `CURLOPT_PROXY`.
    pub fn proxy(&mut self, url: &str) -> Result<(), Error> {
        let url = CString::new(url)?;
        let result = self.setopt_str(curl_sys::CURLOPT_PROXY, &url);
        if result.is_ok() {
            self.inner.proxy_url = Some(url.to_string_lossy().into_owned());
            self.inner.explicit_proxy = true;
            self.inner.client_config_id = None;
        }
        result
    }

    /// Provide port number the proxy is listening on.
    ///
    /// By default this option is not set (the default port for the proxy
    /// protocol is used) and corresponds to `CURLOPT_PROXYPORT`.
    /// An explicit port in the proxy URL takes precedence over this option.
    pub fn proxy_port(&mut self, port: u16) -> Result<(), Error> {
        let result = self.setopt_long(curl_sys::CURLOPT_PROXYPORT, port as c_long);
        if result.is_ok() {
            self.inner.proxy_port = Some(port);
            self.inner.explicit_proxy = true;
            self.inner.client_config_id = None;
        }
        result
    }

    /// Set CA certificate to verify peer against for proxy.
    ///
    /// By default this value is not set and corresponds to
    /// `CURLOPT_PROXY_CAINFO`.
    pub fn proxy_cainfo(&mut self, cainfo: &str) -> Result<(), Error> {
        let cainfo = CString::new(cainfo)?;
        self.setopt_str(curl_sys::CURLOPT_PROXY_CAINFO, &cainfo)
    }

    /// Specify a directory holding CA certificates for proxy.
    ///
    /// The specified directory should hold multiple CA certificates to verify
    /// the HTTPS proxy with. If libcurl is built against OpenSSL, the
    /// certificate directory must be prepared using the OpenSSL `c_rehash`
    /// utility.
    ///
    /// By default this value is not set and corresponds to
    /// `CURLOPT_PROXY_CAPATH`.
    pub fn proxy_capath<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_PROXY_CAPATH, path.as_ref())
    }

    /// Set client certificate for proxy.
    ///
    /// By default this value is not set and corresponds to
    /// `CURLOPT_PROXY_SSLCERT`.
    pub fn proxy_sslcert(&mut self, sslcert: &str) -> Result<(), Error> {
        let sslcert = CString::new(sslcert)?;
        self.setopt_str(curl_sys::CURLOPT_PROXY_SSLCERT, &sslcert)
    }

    /// Specify type of the client SSL certificate for HTTPS proxy.
    ///
    /// The string should be the format of your certificate. Supported formats
    /// are "PEM" and "DER", except with Secure Transport. OpenSSL (versions
    /// 0.9.3 and later) and Secure Transport (on iOS 5 or later, or OS X 10.7
    /// or later) also support "P12" for PKCS#12-encoded files.
    ///
    /// By default this option is "PEM" and corresponds to
    /// `CURLOPT_PROXY_SSLCERTTYPE`.
    pub fn proxy_sslcert_type(&mut self, kind: &str) -> Result<(), Error> {
        let kind = CString::new(kind)?;
        self.setopt_str(curl_sys::CURLOPT_PROXY_SSLCERTTYPE, &kind)
    }

    /// Set the client certificate for the proxy using an in-memory blob.
    ///
    /// The specified byte buffer should contain the binary content of the
    /// certificate, which will be copied into the handle.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXY_SSLCERT_BLOB`.
    pub fn proxy_sslcert_blob(&mut self, blob: &[u8]) -> Result<(), Error> {
        self.setopt_blob(curl_sys::CURLOPT_PROXY_SSLCERT_BLOB, blob)
    }

    /// Set private key for HTTPS proxy.
    ///
    /// By default this value is not set and corresponds to
    /// `CURLOPT_PROXY_SSLKEY`.
    pub fn proxy_sslkey(&mut self, sslkey: &str) -> Result<(), Error> {
        let sslkey = CString::new(sslkey)?;
        self.setopt_str(curl_sys::CURLOPT_PROXY_SSLKEY, &sslkey)
    }

    /// Set type of the private key file for HTTPS proxy.
    ///
    /// The string should be the format of your private key. Supported formats
    /// are "PEM", "DER" and "ENG".
    ///
    /// The format "ENG" enables you to load the private key from a crypto
    /// engine. In this case `ssl_key` is used as an identifier passed to
    /// the engine. You have to set the crypto engine with `ssl_engine`.
    /// "DER" format key file currently does not work because of a bug in
    /// OpenSSL.
    ///
    /// By default this option is "PEM" and corresponds to
    /// `CURLOPT_PROXY_SSLKEYTYPE`.
    pub fn proxy_sslkey_type(&mut self, kind: &str) -> Result<(), Error> {
        let kind = CString::new(kind)?;
        self.setopt_str(curl_sys::CURLOPT_PROXY_SSLKEYTYPE, &kind)
    }

    /// Set the private key for the proxy using an in-memory blob.
    ///
    /// The specified byte buffer should contain the binary content of the
    /// private key, which will be copied into the handle.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXY_SSLKEY_BLOB`.
    pub fn proxy_sslkey_blob(&mut self, blob: &[u8]) -> Result<(), Error> {
        self.setopt_blob(curl_sys::CURLOPT_PROXY_SSLKEY_BLOB, blob)
    }

    /// Set passphrase to private key for HTTPS proxy.
    ///
    /// This will be used as the password required to use the `ssl_key`.
    /// You never needed a pass phrase to load a certificate but you need one to
    /// load your private key.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXY_KEYPASSWD`.
    pub fn proxy_key_password(&mut self, password: &str) -> Result<(), Error> {
        let password = CString::new(password)?;
        self.setopt_str(curl_sys::CURLOPT_PROXY_KEYPASSWD, &password)
    }

    /// Indicates the type of proxy being used.
    ///
    /// By default this option is `ProxyType::Http` and corresponds to
    /// `CURLOPT_PROXYTYPE`.
    pub fn proxy_type(&mut self, kind: ProxyType) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_PROXYTYPE, kind as c_long)
    }

    /// Provide a list of hosts that should not be proxied to.
    ///
    /// This string is a comma-separated list of hosts which should not use the
    /// proxy specified for connections. A single `*` character is also accepted
    /// as a wildcard for all hosts.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_NOPROXY`.
    pub fn noproxy(&mut self, skip: &str) -> Result<(), Error> {
        let skip = CString::new(skip)?;
        self.setopt_str(curl_sys::CURLOPT_NOPROXY, &skip)
    }

    /// Inform curl whether it should tunnel all operations through the proxy.
    ///
    /// This essentially means that a `CONNECT` is sent to the proxy for all
    /// outbound requests.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_HTTPPROXYTUNNEL`.
    pub fn http_proxy_tunnel(&mut self, tunnel: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_HTTPPROXYTUNNEL, tunnel as c_long)
    }

    /// Tell curl which interface to bind to for an outgoing network interface.
    ///
    /// The interface name, IP address, or host name can be specified here.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_INTERFACE`.
    pub fn interface(&mut self, interface: &str) -> Result<(), Error> {
        let s = CString::new(interface)?;
        self.setopt_str(curl_sys::CURLOPT_INTERFACE, &s)
    }

    /// Indicate which port should be bound to locally for this connection.
    ///
    /// By default this option is 0 (any port) and corresponds to
    /// `CURLOPT_LOCALPORT`.
    pub fn set_local_port(&mut self, port: u16) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_LOCALPORT, port as c_long)
    }

    /// Indicates the number of attempts libcurl will perform to find a working
    /// port number.
    ///
    /// By default this option is 1 and corresponds to
    /// `CURLOPT_LOCALPORTRANGE`.
    pub fn local_port_range(&mut self, range: u16) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_LOCALPORTRANGE, range as c_long)
    }

    /// Sets the timeout of how long name resolves will be kept in memory.
    ///
    /// This is distinct from DNS TTL options and is entirely speculative.
    ///
    /// By default this option is 60s and corresponds to
    /// `CURLOPT_DNS_CACHE_TIMEOUT`.
    pub fn dns_cache_timeout(&mut self, dur: Duration) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_DNS_CACHE_TIMEOUT, dur.as_secs() as c_long)
    }

    /// Specify the preferred receive buffer size, in bytes.
    ///
    /// This is treated as a request, not an order, and the main point of this
    /// is that the write callback may get called more often with smaller
    /// chunks.
    ///
    /// By default this option is the maximum write size and corresopnds to
    /// `CURLOPT_BUFFERSIZE`.
    pub fn buffer_size(&mut self, size: usize) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_BUFFERSIZE, size as c_long)
    }

    /// Specify the preferred send buffer size, in bytes.
    ///
    /// This is treated as a request, not an order, and the main point of this
    /// is that the read callback may get called more often with smaller
    /// chunks.
    ///
    /// The upload buffer size is by default 64 kilobytes.
    pub fn upload_buffer_size(&mut self, size: usize) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_UPLOAD_BUFFERSIZE, size as c_long)
    }

    /// Configures whether the TCP_NODELAY option is set, or Nagle's algorithm
    /// is disabled.
    ///
    /// The purpose of Nagle's algorithm is to minimize the number of small
    /// packet's on the network, and disabling this may be less efficient in
    /// some situations.
    ///
    /// By default this option is `true` and corresponds to
    /// `CURLOPT_TCP_NODELAY`.
    pub fn tcp_nodelay(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_TCP_NODELAY, enable as c_long)
    }

    /// Configures whether TCP keepalive probes will be sent.
    ///
    /// The delay and frequency of these probes is controlled by `tcp_keepidle`
    /// and `tcp_keepintvl`.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_TCP_KEEPALIVE`.
    pub fn tcp_keepalive(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_TCP_KEEPALIVE, enable as c_long)
    }

    /// Configures the TCP keepalive idle time wait.
    ///
    /// This is the delay, after which the connection is idle, keepalive probes
    /// will be sent. Not all operating systems support this.
    ///
    /// By default this corresponds to `CURLOPT_TCP_KEEPIDLE`.
    pub fn tcp_keepidle(&mut self, amt: Duration) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_TCP_KEEPIDLE, amt.as_secs() as c_long)
    }

    /// Configures the delay between keepalive probes.
    ///
    /// By default this corresponds to `CURLOPT_TCP_KEEPINTVL`.
    pub fn tcp_keepintvl(&mut self, amt: Duration) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_TCP_KEEPINTVL, amt.as_secs() as c_long)
    }

    /// Configures the scope for local IPv6 addresses.
    ///
    /// Sets the scope_id value to use when connecting to IPv6 or link-local
    /// addresses.
    ///
    /// By default this value is 0 and corresponds to `CURLOPT_ADDRESS_SCOPE`
    pub fn address_scope(&mut self, scope: u32) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_ADDRESS_SCOPE, scope as c_long)
    }

    // =========================================================================
    // Names and passwords

    /// Configures the username to pass as authentication for this connection.
    ///
    /// By default this value is not set and corresponds to `CURLOPT_USERNAME`.
    pub fn username(&mut self, user: &str) -> Result<(), Error> {
        let user = CString::new(user)?;
        self.setopt_str(curl_sys::CURLOPT_USERNAME, &user)
    }

    /// Configures the password to pass as authentication for this connection.
    ///
    /// By default this value is not set and corresponds to `CURLOPT_PASSWORD`.
    pub fn password(&mut self, pass: &str) -> Result<(), Error> {
        let pass = CString::new(pass)?;
        self.setopt_str(curl_sys::CURLOPT_PASSWORD, &pass)
    }

    /// Set HTTP server authentication methods to try
    ///
    /// If more than one method is set, libcurl will first query the site to see
    /// which authentication methods it supports and then pick the best one you
    /// allow it to use. For some methods, this will induce an extra network
    /// round-trip. Set the actual name and password with the `password` and
    /// `username` methods.
    ///
    /// For authentication with a proxy, see `proxy_auth`.
    ///
    /// By default this value is basic and corresponds to `CURLOPT_HTTPAUTH`.
    pub fn http_auth(&mut self, auth: &Auth) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_HTTPAUTH, auth.bits)
    }

    /// Configures the proxy username to pass as authentication for this
    /// connection.
    ///
    /// By default this value is not set and corresponds to
    /// `CURLOPT_PROXYUSERNAME`.
    pub fn proxy_username(&mut self, user: &str) -> Result<(), Error> {
        let user = CString::new(user)?;
        self.setopt_str(curl_sys::CURLOPT_PROXYUSERNAME, &user)
    }

    /// Configures the proxy password to pass as authentication for this
    /// connection.
    ///
    /// By default this value is not set and corresponds to
    /// `CURLOPT_PROXYPASSWORD`.
    pub fn proxy_password(&mut self, pass: &str) -> Result<(), Error> {
        let pass = CString::new(pass)?;
        self.setopt_str(curl_sys::CURLOPT_PROXYPASSWORD, &pass)
    }

    /// Set HTTP proxy authentication methods to try
    ///
    /// If more than one method is set, libcurl will first query the site to see
    /// which authentication methods it supports and then pick the best one you
    /// allow it to use. For some methods, this will induce an extra network
    /// round-trip. Set the actual name and password with the `proxy_password`
    /// and `proxy_username` methods.
    ///
    /// By default this value is basic and corresponds to `CURLOPT_PROXYAUTH`.
    pub fn proxy_auth(&mut self, auth: &Auth) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_PROXYAUTH, auth.bits)
    }

    // =========================================================================
    // HTTP Options

    /// Enables automatic decompression of HTTP downloads.
    ///
    /// Sets the contents of the Accept-Encoding header sent in an HTTP request.
    /// This enables decoding of a response with Content-Encoding.
    ///
    /// Currently supported encoding are `identity`, `zlib`, and `gzip`. A
    /// zero-length string passed in will send all accepted encodings.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_ACCEPT_ENCODING`.
    pub fn accept_encoding(&mut self, encoding: &str) -> Result<(), Error> {
        let encoding = CString::new(encoding)?;
        self.setopt_str(curl_sys::CURLOPT_ACCEPT_ENCODING, &encoding)
    }

    /// Request the HTTP Transfer Encoding.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_TRANSFER_ENCODING`.
    pub fn transfer_encoding(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_TRANSFER_ENCODING, enable as c_long)
    }

    /// Make an HTTP PUT request.
    ///
    /// By default this option is `false` and corresponds to `CURLOPT_PUT`.
    pub fn put(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_PUT, enable as c_long)
    }

    /// Make an HTTP POST request.
    ///
    /// This will also make the library use the
    /// `Content-Type: application/x-www-form-urlencoded` header.
    ///
    /// POST data can be specified through `post_fields` or by specifying a read
    /// function.
    ///
    /// By default this option is `false` and corresponds to `CURLOPT_POST`.
    pub fn post(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_POST, enable as c_long)
    }

    /// Configures the data that will be uploaded as part of a POST.
    ///
    /// Note that the data is copied into this handle and if that's not desired
    /// then the read callbacks can be used instead.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_COPYPOSTFIELDS`.
    pub fn post_fields_copy(&mut self, data: &[u8]) -> Result<(), Error> {
        // Set the length before the pointer so libcurl knows how much to read
        self.post_field_size(data.len() as u64)?;
        self.setopt_ptr(curl_sys::CURLOPT_COPYPOSTFIELDS, data.as_ptr() as *const _)
    }

    /// Configures the size of data that's going to be uploaded as part of a
    /// POST operation.
    ///
    /// This is called automatically as part of `post_fields` and should only
    /// be called if data is being provided in a read callback (and even then
    /// it's optional).
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_POSTFIELDSIZE_LARGE`.
    pub fn post_field_size(&mut self, size: u64) -> Result<(), Error> {
        // Clear anything previous to ensure we don't read past a buffer
        self.setopt_ptr(curl_sys::CURLOPT_POSTFIELDS, ptr::null())?;
        self.setopt_off_t(
            curl_sys::CURLOPT_POSTFIELDSIZE_LARGE,
            size as curl_sys::curl_off_t,
        )
    }

    /// Tells libcurl you want a multipart/formdata HTTP POST to be made and you
    /// instruct what data to pass on to the server in the `form` argument.
    ///
    /// By default this option is set to null and corresponds to
    /// `CURLOPT_HTTPPOST`.
    pub fn httppost(&mut self, form: Form) -> Result<(), Error> {
        self.setopt_ptr(curl_sys::CURLOPT_HTTPPOST, form::raw(&form) as *const _)?;
        self.inner.form = Some(form);
        Ok(())
    }

    /// Sets the HTTP referer header
    ///
    /// By default this option is not set and corresponds to `CURLOPT_REFERER`.
    pub fn referer(&mut self, referer: &str) -> Result<(), Error> {
        let referer = CString::new(referer)?;
        self.setopt_str(curl_sys::CURLOPT_REFERER, &referer)
    }

    /// Sets the HTTP user-agent header
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_USERAGENT`.
    pub fn useragent(&mut self, useragent: &str) -> Result<(), Error> {
        let useragent = CString::new(useragent)?;
        self.setopt_str(curl_sys::CURLOPT_USERAGENT, &useragent)
    }

    /// Add some headers to this HTTP request.
    ///
    /// If you add a header that is otherwise used internally, the value here
    /// takes precedence. If a header is added with no content (like `Accept:`)
    /// the internally the header will get disabled. To add a header with no
    /// content, use the form `MyHeader;` (not the trailing semicolon).
    ///
    /// Headers must not be CRLF terminated. Many replaced headers have common
    /// shortcuts which should be prefered.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_HTTPHEADER`
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use curl::easy::{Easy, List};
    ///
    /// let mut list = List::new();
    /// list.append("Foo: bar").unwrap();
    /// list.append("Bar: baz").unwrap();
    ///
    /// let mut handle = Easy::new();
    /// handle.url("https://www.rust-lang.org/").unwrap();
    /// handle.http_headers(list).unwrap();
    /// let mut event_loop = curl::EventLoop::new().unwrap();
    /// event_loop.add(handle, |_, _| {}).unwrap();
    /// event_loop.run().unwrap();
    /// ```
    pub fn http_headers(&mut self, list: List) -> Result<(), Error> {
        let ptr = list::raw(&list);
        let result = self.setopt_ptr(curl_sys::CURLOPT_HTTPHEADER, ptr as *const _);
        if result.is_ok() {
            self.inner.header_list = Some(list);
            self.inner.explicit_headers = true;
            self.inner.client_config_id = None;
        }
        result
    }

    /// Add some headers to send to the HTTP proxy.
    ///
    /// This function is essentially the same as `http_headers`.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXYHEADER`
    pub fn proxy_headers(&mut self, list: List) -> Result<(), Error> {
        let ptr = list::raw(&list);
        self.inner.proxy_header_list = Some(list);
        self.setopt_ptr(curl_sys::CURLOPT_PROXYHEADER, ptr as *const _)
    }

    /// Set the contents of the HTTP Cookie header.
    ///
    /// Pass a string of the form `name=contents` for one cookie value or
    /// `name1=val1; name2=val2` for multiple values.
    ///
    /// Using this option multiple times will only make the latest string
    /// override the previous ones. This option will not enable the cookie
    /// engine, use `cookie_file` or `cookie_jar` to do that.
    ///
    /// By default this option is not set and corresponds to `CURLOPT_COOKIE`.
    pub fn cookie(&mut self, cookie: &str) -> Result<(), Error> {
        let cookie = CString::new(cookie)?;
        self.setopt_str(curl_sys::CURLOPT_COOKIE, &cookie)
    }

    /// Set the file name to read cookies from.
    ///
    /// The cookie data can be in either the old Netscape / Mozilla cookie data
    /// format or just regular HTTP headers (Set-Cookie style) dumped to a file.
    ///
    /// This also enables the cookie engine, making libcurl parse and send
    /// cookies on subsequent requests with this handle.
    ///
    /// Given an empty or non-existing file or by passing the empty string ("")
    /// to this option, you can enable the cookie engine without reading any
    /// initial cookies.
    ///
    /// If you use this option multiple times, you just add more files to read.
    /// Subsequent files will add more cookies.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_COOKIEFILE`.
    pub fn cookie_file<P: AsRef<Path>>(&mut self, file: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_COOKIEFILE, file.as_ref())
    }

    /// Set the file name to store cookies to.
    ///
    /// This will make libcurl write all internally known cookies to the file
    /// when this handle is dropped. If no cookies are known, no file will be
    /// created. Specify "-" as filename to instead have the cookies written to
    /// stdout. Using this option also enables cookies for this session, so if
    /// you for example follow a location it will make matching cookies get sent
    /// accordingly.
    ///
    /// Note that libcurl doesn't read any cookies from the cookie jar. If you
    /// want to read cookies from a file, use `cookie_file`.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_COOKIEJAR`.
    pub fn cookie_jar<P: AsRef<Path>>(&mut self, file: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_COOKIEJAR, file.as_ref())
    }

    /// Start a new cookie session
    ///
    /// Marks this as a new cookie "session". It will force libcurl to ignore
    /// all cookies it is about to load that are "session cookies" from the
    /// previous session. By default, libcurl always stores and loads all
    /// cookies, independent if they are session cookies or not. Session cookies
    /// are cookies without expiry date and they are meant to be alive and
    /// existing for this "session" only.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_COOKIESESSION`.
    pub fn cookie_session(&mut self, session: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_COOKIESESSION, session as c_long)
    }

    /// Add to or manipulate cookies held in memory.
    ///
    /// Such a cookie can be either a single line in Netscape / Mozilla format
    /// or just regular HTTP-style header (Set-Cookie: ...) format. This will
    /// also enable the cookie engine. This adds that single cookie to the
    /// internal cookie store.
    ///
    /// Exercise caution if you are using this option and multiple transfers may
    /// occur. If you use the Set-Cookie format and don't specify a domain then
    /// the cookie is sent for any domain (even after redirects are followed)
    /// and cannot be modified by a server-set cookie. If a server sets a cookie
    /// of the same name (or maybe you've imported one) then both will be sent
    /// on a future transfer to that server, likely not what you intended.
    /// address these issues set a domain in Set-Cookie or use the Netscape
    /// format.
    ///
    /// Additionally, there are commands available that perform actions if you
    /// pass in these exact strings:
    ///
    /// * "ALL" - erases all cookies held in memory
    /// * "SESS" - erases all session cookies held in memory
    /// * "FLUSH" - write all known cookies to the specified cookie jar
    /// * "RELOAD" - reread all cookies from the cookie file
    ///
    /// By default this options corresponds to `CURLOPT_COOKIELIST`
    pub fn cookie_list(&mut self, cookie: &str) -> Result<(), Error> {
        let cookie = CString::new(cookie)?;
        self.setopt_str(curl_sys::CURLOPT_COOKIELIST, &cookie)
    }

    /// Ask for a HTTP GET request.
    ///
    /// By default this option is `false` and corresponds to `CURLOPT_HTTPGET`.
    pub fn get(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_HTTPGET, enable as c_long)
    }

    /// Ignore the content-length header.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_IGNORE_CONTENT_LENGTH`.
    pub fn ignore_content_length(&mut self, ignore: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_IGNORE_CONTENT_LENGTH, ignore as c_long)
    }

    /// Enable or disable HTTP content decoding.
    ///
    /// By default this option is `true` and corresponds to
    /// `CURLOPT_HTTP_CONTENT_DECODING`.
    pub fn http_content_decoding(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_HTTP_CONTENT_DECODING, enable as c_long)
    }

    /// Enable or disable HTTP transfer decoding.
    ///
    /// By default this option is `true` and corresponds to
    /// `CURLOPT_HTTP_TRANSFER_DECODING`.
    pub fn http_transfer_decoding(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_HTTP_TRANSFER_DECODING, enable as c_long)
    }

    // =========================================================================
    // Protocol Options

    /// Indicates the range that this request should retrieve.
    ///
    /// The string provided should be of the form `N-M` where either `N` or `M`
    /// can be left out. For HTTP transfers multiple ranges separated by commas
    /// are also accepted.
    ///
    /// By default this option is not set and corresponds to `CURLOPT_RANGE`.
    pub fn range(&mut self, range: &str) -> Result<(), Error> {
        let range = CString::new(range)?;
        self.setopt_str(curl_sys::CURLOPT_RANGE, &range)
    }

    /// Set a point to resume transfer from
    ///
    /// Specify the offset in bytes you want the transfer to start from.
    ///
    /// By default this option is 0 and corresponds to
    /// `CURLOPT_RESUME_FROM_LARGE`.
    pub fn resume_from(&mut self, from: u64) -> Result<(), Error> {
        self.setopt_off_t(
            curl_sys::CURLOPT_RESUME_FROM_LARGE,
            from as curl_sys::curl_off_t,
        )
    }

    /// Set a custom request string
    ///
    /// Specifies that a custom request will be made (e.g. a custom HTTP
    /// method). This does not change how libcurl performs internally, just
    /// changes the string sent to the server.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_CUSTOMREQUEST`.
    pub fn custom_request(&mut self, request: &str) -> Result<(), Error> {
        let request = CString::new(request)?;
        self.setopt_str(curl_sys::CURLOPT_CUSTOMREQUEST, &request)
    }

    /// Get the modification time of the remote resource
    ///
    /// If true, libcurl will attempt to get the modification time of the
    /// remote document in this operation. This requires that the remote server
    /// sends the time or replies to a time querying command. The `filetime`
    /// function can be used after a transfer to extract the received time (if
    /// any).
    ///
    /// By default this option is `false` and corresponds to `CURLOPT_FILETIME`
    pub fn fetch_filetime(&mut self, fetch: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_FILETIME, fetch as c_long)
    }

    /// Indicate whether to download the request without getting the body
    ///
    /// This is useful, for example, for doing a HEAD request.
    ///
    /// By default this option is `false` and corresponds to `CURLOPT_NOBODY`.
    pub fn nobody(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_NOBODY, enable as c_long)
    }

    /// Set the size of the input file to send off.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_INFILESIZE_LARGE`.
    pub fn in_filesize(&mut self, size: u64) -> Result<(), Error> {
        self.setopt_off_t(
            curl_sys::CURLOPT_INFILESIZE_LARGE,
            size as curl_sys::curl_off_t,
        )
    }

    /// Enable or disable data upload.
    ///
    /// This means that a PUT request will be made for HTTP and probably wants
    /// to be combined with the read callback as well as the `in_filesize`
    /// method.
    ///
    /// By default this option is `false` and corresponds to `CURLOPT_UPLOAD`.
    pub fn upload(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_UPLOAD, enable as c_long)
    }

    /// Configure the maximum file size to download.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_MAXFILESIZE_LARGE`.
    pub fn max_filesize(&mut self, size: u64) -> Result<(), Error> {
        self.setopt_off_t(
            curl_sys::CURLOPT_MAXFILESIZE_LARGE,
            size as curl_sys::curl_off_t,
        )
    }

    /// Selects a condition for a time request.
    ///
    /// This value indicates how the `time_value` option is interpreted.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_TIMECONDITION`.
    pub fn time_condition(&mut self, cond: TimeCondition) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_TIMECONDITION, cond as c_long)
    }

    /// Sets the time value for a conditional request.
    ///
    /// The value here should be the number of seconds elapsed since January 1,
    /// 1970. To pass how to interpret this value, use `time_condition`.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_TIMEVALUE`.
    pub fn time_value(&mut self, val: i64) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_TIMEVALUE, val as c_long)
    }

    // =========================================================================
    // Connection Options

    /// Set maximum time the request is allowed to take.
    ///
    /// Normally, name lookups can take a considerable time and limiting
    /// operations to less than a few minutes risk aborting perfectly normal
    /// operations.
    ///
    /// If libcurl is built to use the standard system name resolver, that
    /// portion of the transfer will still use full-second resolution for
    /// timeouts with a minimum timeout allowed of one second.
    ///
    /// In unix-like systems, this might cause signals to be used unless
    /// `nosignal` is set.
    ///
    /// Since this puts a hard limit for how long a request is allowed to
    /// take, it has limited use in dynamic use cases with varying transfer
    /// times. You are then advised to explore `low_speed_limit`,
    /// `low_speed_time` or using `progress_function` to implement your own
    /// timeout logic.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_TIMEOUT_MS`.
    pub fn timeout(&mut self, timeout: Duration) -> Result<(), Error> {
        let ms = timeout.as_millis();
        match c_long::try_from(ms) {
            Ok(amt) => {
                let r = self.setopt_long(curl_sys::CURLOPT_TIMEOUT_MS, amt);
                if r.is_ok() {
                    self.inner.explicit_timeout = true;
                    self.inner.client_config_id = None;
                }
                r
            }
            Err(_) => {
                let amt = c_long::try_from(ms / 1000)
                    .map_err(|_| Error::new(curl_sys::CURLE_BAD_FUNCTION_ARGUMENT))?;
                let r = self.setopt_long(curl_sys::CURLOPT_TIMEOUT, amt);
                if r.is_ok() {
                    self.inner.explicit_timeout = true;
                    self.inner.client_config_id = None;
                }
                r
            }
        }
    }

    /// Set the low speed limit in bytes per second.
    ///
    /// This specifies the average transfer speed in bytes per second that the
    /// transfer should be below during `low_speed_time` for libcurl to consider
    /// it to be too slow and abort.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_LOW_SPEED_LIMIT`.
    pub fn low_speed_limit(&mut self, limit: u32) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_LOW_SPEED_LIMIT, limit as c_long)
    }

    /// Set the low speed time period.
    ///
    /// Specifies the window of time for which if the transfer rate is below
    /// `low_speed_limit` the request will be aborted.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_LOW_SPEED_TIME`.
    pub fn low_speed_time(&mut self, dur: Duration) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_LOW_SPEED_TIME, dur.as_secs() as c_long)
    }

    /// Rate limit data upload speed
    ///
    /// If an upload exceeds this speed (counted in bytes per second) on
    /// cumulative average during the transfer, the transfer will pause to keep
    /// the average rate less than or equal to the parameter value.
    ///
    /// By default this option is not set (unlimited speed) and corresponds to
    /// `CURLOPT_MAX_SEND_SPEED_LARGE`.
    pub fn max_send_speed(&mut self, speed: u64) -> Result<(), Error> {
        self.setopt_off_t(
            curl_sys::CURLOPT_MAX_SEND_SPEED_LARGE,
            speed as curl_sys::curl_off_t,
        )
    }

    /// Rate limit data download speed
    ///
    /// If a download exceeds this speed (counted in bytes per second) on
    /// cumulative average during the transfer, the transfer will pause to keep
    /// the average rate less than or equal to the parameter value.
    ///
    /// By default this option is not set (unlimited speed) and corresponds to
    /// `CURLOPT_MAX_RECV_SPEED_LARGE`.
    pub fn max_recv_speed(&mut self, speed: u64) -> Result<(), Error> {
        self.setopt_off_t(
            curl_sys::CURLOPT_MAX_RECV_SPEED_LARGE,
            speed as curl_sys::curl_off_t,
        )
    }

    /// Set the maximum connection cache size.
    ///
    /// The set amount will be the maximum number of simultaneously open
    /// persistent connections that libcurl may cache in the pool associated
    /// with this handle. The default is 5, and there isn't much point in
    /// changing this value unless you are perfectly aware of how this works and
    /// changes libcurl's behaviour. This concerns connections using any of the
    /// protocols that support persistent connections.
    ///
    /// When reaching the maximum limit, curl closes the oldest one in the cache
    /// to prevent increasing the number of open connections.
    ///
    /// By default this option is set to 5 and corresponds to
    /// `CURLOPT_MAXCONNECTS`
    pub fn max_connects(&mut self, max: u32) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_MAXCONNECTS, max as c_long)
    }

    /// Set the maximum idle time allowed for a connection.
    ///
    /// This configuration sets the maximum time that a connection inside of the connection cache
    /// can be reused. Any connection older than this value will be considered stale and will
    /// be closed.
    ///
    /// By default, a value of 118 seconds is used.
    pub fn maxage_conn(&mut self, max_age: Duration) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_MAXAGE_CONN, max_age.as_secs() as c_long)
    }

    /// Force a new connection to be used.
    ///
    /// Makes the next transfer use a new (fresh) connection by force instead of
    /// trying to re-use an existing one. This option should be used with
    /// caution and only if you understand what it does as it may seriously
    /// impact performance.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_FRESH_CONNECT`.
    pub fn fresh_connect(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_FRESH_CONNECT, enable as c_long)
    }

    /// Make connection get closed at once after use.
    ///
    /// Makes libcurl explicitly close the connection when done with the
    /// transfer. Normally, libcurl keeps all connections alive when done with
    /// one transfer in case a succeeding one follows that can re-use them.
    /// This option should be used with caution and only if you understand what
    /// it does as it can seriously impact performance.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_FORBID_REUSE`.
    pub fn forbid_reuse(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_FORBID_REUSE, enable as c_long)
    }

    /// Timeout for the connect phase
    ///
    /// This is the maximum time that you allow the connection phase to the
    /// server to take. This only limits the connection phase, it has no impact
    /// once it has connected.
    ///
    /// By default this value is 300 seconds and corresponds to
    /// `CURLOPT_CONNECTTIMEOUT_MS`.
    pub fn connect_timeout(&mut self, timeout: Duration) -> Result<(), Error> {
        let ms = timeout.as_millis();
        match c_long::try_from(ms) {
            Ok(amt) => self.setopt_long(curl_sys::CURLOPT_CONNECTTIMEOUT_MS, amt),
            Err(_) => {
                let amt = c_long::try_from(ms / 1000)
                    .map_err(|_| Error::new(curl_sys::CURLE_BAD_FUNCTION_ARGUMENT))?;
                self.setopt_long(curl_sys::CURLOPT_CONNECTTIMEOUT, amt)
            }
        }
    }

    /// Specify which IP protocol version to use
    ///
    /// Allows an application to select what kind of IP addresses to use when
    /// resolving host names. This is only interesting when using host names
    /// that resolve addresses using more than one version of IP.
    ///
    /// By default this value is "any" and corresponds to `CURLOPT_IPRESOLVE`.
    pub fn ip_resolve(&mut self, resolve: IpResolve) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_IPRESOLVE, resolve as c_long)
    }

    fn resolve(&mut self, list: List) -> Result<(), Error> {
        let ptr = list::raw(&list);
        self.inner.resolve_list = Some(list);
        self.setopt_ptr(curl_sys::CURLOPT_RESOLVE, ptr as *const _)
    }

    pub(crate) fn rearm_resolve(&mut self) -> Result<(), Error> {
        if let Some(list) = self.inner.resolve_list.as_ref() {
            let ptr = list::raw(list);
            self.setopt_ptr(curl_sys::CURLOPT_RESOLVE, ptr as *const _)?;
        }
        Ok(())
    }

    /// Configure whether to stop when connected to target server
    ///
    /// When enabled it tells the library to perform all the required proxy
    /// authentication and connection setup, but no data transfer, and then
    /// return.
    ///
    /// The option can be used to simply test a connection to a server.
    ///
    /// By default this value is `false` and corresponds to
    /// `CURLOPT_CONNECT_ONLY`.
    pub fn connect_only(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_CONNECT_ONLY, enable as c_long)
    }

    /// Configure pull-mode WebSocket behavior.
    pub fn websocket_options(&mut self, raw_mode: bool, no_auto_pong: bool) -> Result<(), Error> {
        let mut bits = 0;
        if raw_mode {
            bits |= curl_sys::CURLWS_RAW_MODE;
        }
        if no_auto_pong {
            bits |= curl_sys::CURLWS_NOAUTOPONG;
        }
        self.setopt_long(curl_sys::CURLOPT_WS_OPTIONS, bits)
    }
    /// Enable libcurl's pull-mode WebSocket connection.
    pub fn websocket_connect_only(&mut self) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_CONNECT_ONLY, 2)
    }

    // =========================================================================
    // SSL/Security Options

    /// Sets the SSL client certificate.
    ///
    /// The string should be the file name of your client certificate. The
    /// default format is "P12" on Secure Transport and "PEM" on other engines,
    /// and can be changed with `ssl_cert_type`.
    ///
    /// With NSS or Secure Transport, this can also be the nickname of the
    /// certificate you wish to authenticate with as it is named in the security
    /// database. If you want to use a file from the current directory, please
    /// precede it with "./" prefix, in order to avoid confusion with a
    /// nickname.
    ///
    /// When using a client certificate, you most likely also need to provide a
    /// private key with `ssl_key`.
    ///
    /// By default this option is not set and corresponds to `CURLOPT_SSLCERT`.
    pub fn ssl_cert<P: AsRef<Path>>(&mut self, cert: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_SSLCERT, cert.as_ref())
    }

    /// Set the SSL client certificate using an in-memory blob.
    ///
    /// The specified byte buffer should contain the binary content of your
    /// client certificate, which will be copied into the handle. The format of
    /// the certificate can be specified with `ssl_cert_type`.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_SSLCERT_BLOB`.
    pub fn ssl_cert_blob(&mut self, blob: &[u8]) -> Result<(), Error> {
        self.setopt_blob(curl_sys::CURLOPT_SSLCERT_BLOB, blob)
    }

    /// Specify type of the client SSL certificate.
    ///
    /// The string should be the format of your certificate. Supported formats
    /// are "PEM" and "DER", except with Secure Transport. OpenSSL (versions
    /// 0.9.3 and later) and Secure Transport (on iOS 5 or later, or OS X 10.7
    /// or later) also support "P12" for PKCS#12-encoded files.
    ///
    /// By default this option is "PEM" and corresponds to
    /// `CURLOPT_SSLCERTTYPE`.
    pub fn ssl_cert_type(&mut self, kind: &str) -> Result<(), Error> {
        let kind = CString::new(kind)?;
        self.setopt_str(curl_sys::CURLOPT_SSLCERTTYPE, &kind)
    }

    /// Specify private keyfile for TLS and SSL client cert.
    ///
    /// The string should be the file name of your private key. The default
    /// format is "PEM" and can be changed with `ssl_key_type`.
    ///
    /// (iOS and Mac OS X only) This option is ignored if curl was built against
    /// Secure Transport. Secure Transport expects the private key to be already
    /// present in the keychain or PKCS#12 file containing the certificate.
    ///
    /// By default this option is not set and corresponds to `CURLOPT_SSLKEY`.
    pub fn ssl_key<P: AsRef<Path>>(&mut self, key: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_SSLKEY, key.as_ref())
    }

    /// Specify an SSL private key using an in-memory blob.
    ///
    /// The specified byte buffer should contain the binary content of your
    /// private key, which will be copied into the handle. The format of
    /// the private key can be specified with `ssl_key_type`.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_SSLKEY_BLOB`.
    pub fn ssl_key_blob(&mut self, blob: &[u8]) -> Result<(), Error> {
        self.setopt_blob(curl_sys::CURLOPT_SSLKEY_BLOB, blob)
    }

    /// Set type of the private key file.
    ///
    /// The string should be the format of your private key. Supported formats
    /// are "PEM", "DER" and "ENG".
    ///
    /// The format "ENG" enables you to load the private key from a crypto
    /// engine. In this case `ssl_key` is used as an identifier passed to
    /// the engine. You have to set the crypto engine with `ssl_engine`.
    /// "DER" format key file currently does not work because of a bug in
    /// OpenSSL.
    ///
    /// By default this option is "PEM" and corresponds to
    /// `CURLOPT_SSLKEYTYPE`.
    pub fn ssl_key_type(&mut self, kind: &str) -> Result<(), Error> {
        let kind = CString::new(kind)?;
        self.setopt_str(curl_sys::CURLOPT_SSLKEYTYPE, &kind)
    }

    /// Set passphrase to private key.
    ///
    /// This will be used as the password required to use the `ssl_key`.
    /// You never needed a pass phrase to load a certificate but you need one to
    /// load your private key.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_KEYPASSWD`.
    pub fn key_password(&mut self, password: &str) -> Result<(), Error> {
        let password = CString::new(password)?;
        self.setopt_str(curl_sys::CURLOPT_KEYPASSWD, &password)
    }

    /// Set the SSL Certificate Authorities using an in-memory blob.
    ///
    /// The specified byte buffer should contain the binary content of one
    /// or more PEM-encoded CA certificates, which will be copied into
    /// the handle.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_CAINFO_BLOB`.
    pub fn ssl_cainfo_blob(&mut self, blob: &[u8]) -> Result<(), Error> {
        self.disable_native_ca();
        self.setopt_blob(curl_sys::CURLOPT_CAINFO_BLOB, blob)
    }

    /// Set the SSL Certificate Authorities for HTTPS proxies using an in-memory
    /// blob.
    ///
    /// The specified byte buffer should contain the binary content of one
    /// or more PEM-encoded CA certificates, which will be copied into
    /// the handle.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXY_CAINFO_BLOB`.
    pub fn proxy_ssl_cainfo_blob(&mut self, blob: &[u8]) -> Result<(), Error> {
        self.setopt_blob(curl_sys::CURLOPT_PROXY_CAINFO_BLOB, blob)
    }

    /// Set the SSL engine identifier.
    ///
    /// This will be used as the identifier for the crypto engine you want to
    /// use for your private key.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_SSLENGINE`.
    pub fn ssl_engine(&mut self, engine: &str) -> Result<(), Error> {
        let engine = CString::new(engine)?;
        self.setopt_str(curl_sys::CURLOPT_SSLENGINE, &engine)
    }

    /// Make this handle's SSL engine the default.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_SSLENGINE_DEFAULT`.
    pub fn ssl_engine_default(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_SSLENGINE_DEFAULT, enable as c_long)
    }

    /// Set preferred HTTP version.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_HTTP_VERSION`.
    pub fn http_version(&mut self, version: HttpVersion) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_HTTP_VERSION, version as c_long)
    }

    /// Set preferred TLS/SSL version.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_SSLVERSION`.
    pub fn ssl_version(&mut self, version: SslVersion) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_SSLVERSION, version as c_long)
    }

    /// Set preferred TLS/SSL version when connecting to an HTTPS proxy.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXY_SSLVERSION`.
    pub fn proxy_ssl_version(&mut self, version: SslVersion) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_PROXY_SSLVERSION, version as c_long)
    }

    /// Set preferred TLS/SSL version with minimum version and maximum version.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_SSLVERSION`.
    pub fn ssl_min_max_version(
        &mut self,
        min_version: SslVersion,
        max_version: SslVersion,
    ) -> Result<(), Error> {
        let version = (min_version as c_long) | ((max_version as c_long) << 16);
        self.setopt_long(curl_sys::CURLOPT_SSLVERSION, version)
    }

    /// Set preferred TLS/SSL version with minimum version and maximum version
    /// when connecting to an HTTPS proxy.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXY_SSLVERSION`.
    pub fn proxy_ssl_min_max_version(
        &mut self,
        min_version: SslVersion,
        max_version: SslVersion,
    ) -> Result<(), Error> {
        let version = (min_version as c_long) | ((max_version as c_long) << 16);
        self.setopt_long(curl_sys::CURLOPT_PROXY_SSLVERSION, version)
    }

    /// Verify the certificate's name against host.
    ///
    /// This should be disabled with great caution! It basically disables the
    /// security features of SSL if it is disabled.
    ///
    /// By default this option is set to `true` and corresponds to
    /// `CURLOPT_SSL_VERIFYHOST`.
    pub fn ssl_verify_host(&mut self, verify: bool) -> Result<(), Error> {
        let val = if verify { 2 } else { 0 };
        self.setopt_long(curl_sys::CURLOPT_SSL_VERIFYHOST, val)
    }

    /// Verify the certificate's name against host for HTTPS proxy.
    ///
    /// This should be disabled with great caution! It basically disables the
    /// security features of SSL if it is disabled.
    ///
    /// By default this option is set to `true` and corresponds to
    /// `CURLOPT_PROXY_SSL_VERIFYHOST`.
    pub fn proxy_ssl_verify_host(&mut self, verify: bool) -> Result<(), Error> {
        let val = if verify { 2 } else { 0 };
        self.setopt_long(curl_sys::CURLOPT_PROXY_SSL_VERIFYHOST, val)
    }

    /// Verify the peer's SSL certificate.
    ///
    /// This should be disabled with great caution! It basically disables the
    /// security features of SSL if it is disabled.
    ///
    /// By default this option is set to `true` and corresponds to
    /// `CURLOPT_SSL_VERIFYPEER`.
    pub fn ssl_verify_peer(&mut self, verify: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_SSL_VERIFYPEER, verify as c_long)
    }

    /// Verify the peer's SSL certificate for HTTPS proxy.
    ///
    /// This should be disabled with great caution! It basically disables the
    /// security features of SSL if it is disabled.
    ///
    /// By default this option is set to `true` and corresponds to
    /// `CURLOPT_PROXY_SSL_VERIFYPEER`.
    pub fn proxy_ssl_verify_peer(&mut self, verify: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_PROXY_SSL_VERIFYPEER, verify as c_long)
    }

    /// Specify the path to Certificate Authority (CA) bundle
    ///
    /// The file referenced should hold one or more certificates to verify the
    /// peer with.
    ///
    /// This option is by default set to the system path where libcurl's cacert
    /// bundle is assumed to be stored, as established at build time.
    ///
    /// If curl is built against the NSS SSL library, the NSS PEM PKCS#11 module
    /// (libnsspem.so) needs to be available for this option to work properly.
    ///
    /// By default this option is the system defaults, and corresponds to
    /// `CURLOPT_CAINFO`.
    pub fn cainfo<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        self.disable_native_ca();
        let r = self.setopt_path(curl_sys::CURLOPT_CAINFO, path.as_ref());
        if r.is_ok() {
            self.inner.explicit_cainfo = true;
            self.inner.client_config_id = None;
        }
        r
    }

    /// Set the issuer SSL certificate filename
    ///
    /// Specifies a file holding a CA certificate in PEM format. If the option
    /// is set, an additional check against the peer certificate is performed to
    /// verify the issuer is indeed the one associated with the certificate
    /// provided by the option. This additional check is useful in multi-level
    /// PKI where one needs to enforce that the peer certificate is from a
    /// specific branch of the tree.
    ///
    /// This option makes sense only when used in combination with the
    /// [`Easy::ssl_verify_peer`] option. Otherwise, the result of the check is
    /// not considered as failure.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_ISSUERCERT`.
    pub fn issuer_cert<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_ISSUERCERT, path.as_ref())
    }

    /// Set the issuer SSL certificate filename for HTTPS proxies
    ///
    /// Specifies a file holding a CA certificate in PEM format. If the option
    /// is set, an additional check against the peer certificate is performed to
    /// verify the issuer is indeed the one associated with the certificate
    /// provided by the option. This additional check is useful in multi-level
    /// PKI where one needs to enforce that the peer certificate is from a
    /// specific branch of the tree.
    ///
    /// This option makes sense only when used in combination with the
    /// [`Easy::proxy_ssl_verify_peer`] option. Otherwise, the result of the
    /// check is not considered as failure.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXY_ISSUERCERT`.
    pub fn proxy_issuer_cert<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_PROXY_ISSUERCERT, path.as_ref())
    }

    /// Set the issuer SSL certificate using an in-memory blob.
    ///
    /// The specified byte buffer should contain the binary content of a CA
    /// certificate in the PEM format. The certificate will be copied into the
    /// handle.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_ISSUERCERT_BLOB`.
    pub fn issuer_cert_blob(&mut self, blob: &[u8]) -> Result<(), Error> {
        self.setopt_blob(curl_sys::CURLOPT_ISSUERCERT_BLOB, blob)
    }

    /// Set the issuer SSL certificate for HTTPS proxies using an in-memory blob.
    ///
    /// The specified byte buffer should contain the binary content of a CA
    /// certificate in the PEM format. The certificate will be copied into the
    /// handle.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXY_ISSUERCERT_BLOB`.
    pub fn proxy_issuer_cert_blob(&mut self, blob: &[u8]) -> Result<(), Error> {
        self.setopt_blob(curl_sys::CURLOPT_PROXY_ISSUERCERT_BLOB, blob)
    }

    /// Specify directory holding CA certificates
    ///
    /// Names a directory holding multiple CA certificates to verify the peer
    /// with. If libcurl is built against OpenSSL, the certificate directory
    /// must be prepared using the openssl c_rehash utility. This makes sense
    /// only when used in combination with the `ssl_verify_peer` option.
    ///
    /// By default this option is not set and corresponds to `CURLOPT_CAPATH`.
    pub fn capath<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        self.disable_native_ca();
        self.setopt_path(curl_sys::CURLOPT_CAPATH, path.as_ref())
    }

    /// Specify a Certificate Revocation List file
    ///
    /// Names a file with the concatenation of CRL (in PEM format) to use in the
    /// certificate validation that occurs during the SSL exchange.
    ///
    /// When curl is built to use NSS or GnuTLS, there is no way to influence
    /// the use of CRL passed to help in the verification process. When libcurl
    /// is built with OpenSSL support, X509_V_FLAG_CRL_CHECK and
    /// X509_V_FLAG_CRL_CHECK_ALL are both set, requiring CRL check against all
    /// the elements of the certificate chain if a CRL file is passed.
    ///
    /// This option makes sense only when used in combination with the
    /// [`Easy::ssl_verify_peer`] option.
    ///
    /// A specific error code (`is_ssl_crl_badfile`) is defined with the
    /// option. It is returned when the SSL exchange fails because the CRL file
    /// cannot be loaded. A failure in certificate verification due to a
    /// revocation information found in the CRL does not trigger this specific
    /// error.
    ///
    /// By default this option is not set and corresponds to `CURLOPT_CRLFILE`.
    pub fn crlfile<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_CRLFILE, path.as_ref())
    }

    /// Specify a Certificate Revocation List file to use when connecting to an
    /// HTTPS proxy.
    ///
    /// Names a file with the concatenation of CRL (in PEM format) to use in the
    /// certificate validation that occurs during the SSL exchange.
    ///
    /// When curl is built to use NSS or GnuTLS, there is no way to influence
    /// the use of CRL passed to help in the verification process. When libcurl
    /// is built with OpenSSL support, X509_V_FLAG_CRL_CHECK and
    /// X509_V_FLAG_CRL_CHECK_ALL are both set, requiring CRL check against all
    /// the elements of the certificate chain if a CRL file is passed.
    ///
    /// This option makes sense only when used in combination with the
    /// [`Easy::proxy_ssl_verify_peer`] option.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXY_CRLFILE`.
    pub fn proxy_crlfile<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_PROXY_CRLFILE, path.as_ref())
    }

    /// Request SSL certificate information
    ///
    /// Enable libcurl's certificate chain info gatherer. With this enabled,
    /// libcurl will extract lots of information and data about the certificates
    /// in the certificate chain used in the SSL connection.
    ///
    /// By default this option is `false` and corresponds to
    /// `CURLOPT_CERTINFO`.
    pub fn certinfo(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_CERTINFO, enable as c_long)
    }

    /// Set pinned public key.
    ///
    /// Pass a pointer to a zero terminated string as parameter. The string can
    /// be the file name of your pinned public key. The file format expected is
    /// "PEM" or "DER". The string can also be any number of base64 encoded
    /// sha256 hashes preceded by "sha256//" and separated by ";"
    ///
    /// When negotiating a TLS or SSL connection, the server sends a certificate
    /// indicating its identity. A public key is extracted from this certificate
    /// and if it does not exactly match the public key provided to this option,
    /// curl will abort the connection before sending or receiving any data.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PINNEDPUBLICKEY`.
    pub fn pinned_public_key(&mut self, pubkey: &str) -> Result<(), Error> {
        let key = CString::new(pubkey)?;
        self.setopt_str(curl_sys::CURLOPT_PINNEDPUBLICKEY, &key)
    }

    /// Specify a source for random data
    ///
    /// The file will be used to read from to seed the random engine for SSL and
    /// more.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_RANDOM_FILE`.
    pub fn random_file<P: AsRef<Path>>(&mut self, p: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_RANDOM_FILE, p.as_ref())
    }

    /// Specify EGD socket path.
    ///
    /// Indicates the path name to the Entropy Gathering Daemon socket. It will
    /// be used to seed the random engine for SSL.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_EGDSOCKET`.
    pub fn egd_socket<P: AsRef<Path>>(&mut self, p: P) -> Result<(), Error> {
        self.setopt_path(curl_sys::CURLOPT_EGDSOCKET, p.as_ref())
    }

    /// Specify ciphers to use for TLS.
    ///
    /// Holds the list of ciphers to use for the SSL connection. The list must
    /// be syntactically correct, it consists of one or more cipher strings
    /// separated by colons. Commas or spaces are also acceptable separators
    /// but colons are normally used, !, - and + can be used as operators.
    ///
    /// For OpenSSL and GnuTLS valid examples of cipher lists include 'RC4-SHA',
    /// ´SHA1+DES´, 'TLSv1' and 'DEFAULT'. The default list is normally set when
    /// you compile OpenSSL.
    ///
    /// You'll find more details about cipher lists on this URL:
    ///
    /// <https://www.openssl.org/docs/apps/ciphers.html>
    ///
    /// For NSS, valid examples of cipher lists include 'rsa_rc4_128_md5',
    /// ´rsa_aes_128_sha´, etc. With NSS you don't add/remove ciphers. If one
    /// uses this option then all known ciphers are disabled and only those
    /// passed in are enabled.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_SSL_CIPHER_LIST`.
    pub fn ssl_cipher_list(&mut self, ciphers: &str) -> Result<(), Error> {
        let ciphers = CString::new(ciphers)?;
        self.setopt_str(curl_sys::CURLOPT_SSL_CIPHER_LIST, &ciphers)
    }

    /// Specify ciphers to use for TLS for an HTTPS proxy.
    ///
    /// Holds the list of ciphers to use for the SSL connection. The list must
    /// be syntactically correct, it consists of one or more cipher strings
    /// separated by colons. Commas or spaces are also acceptable separators
    /// but colons are normally used, !, - and + can be used as operators.
    ///
    /// For OpenSSL and GnuTLS valid examples of cipher lists include 'RC4-SHA',
    /// ´SHA1+DES´, 'TLSv1' and 'DEFAULT'. The default list is normally set when
    /// you compile OpenSSL.
    ///
    /// You'll find more details about cipher lists on this URL:
    ///
    /// <https://www.openssl.org/docs/apps/ciphers.html>
    ///
    /// For NSS, valid examples of cipher lists include 'rsa_rc4_128_md5',
    /// ´rsa_aes_128_sha´, etc. With NSS you don't add/remove ciphers. If one
    /// uses this option then all known ciphers are disabled and only those
    /// passed in are enabled.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_PROXY_SSL_CIPHER_LIST`.
    pub fn proxy_ssl_cipher_list(&mut self, ciphers: &str) -> Result<(), Error> {
        let ciphers = CString::new(ciphers)?;
        self.setopt_str(curl_sys::CURLOPT_PROXY_SSL_CIPHER_LIST, &ciphers)
    }

    /// Enable or disable use of the SSL session-ID cache
    ///
    /// By default all transfers are done using the cache enabled. While nothing
    /// ever should get hurt by attempting to reuse SSL session-IDs, there seem
    /// to be or have been broken SSL implementations in the wild that may
    /// require you to disable this in order for you to succeed.
    ///
    /// This corresponds to the `CURLOPT_SSL_SESSIONID_CACHE` option.
    pub fn ssl_sessionid_cache(&mut self, enable: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_SSL_SESSIONID_CACHE, enable as c_long)
    }

    /// Set SSL behavior options
    ///
    /// Inform libcurl about SSL specific behaviors.
    ///
    /// This corresponds to the `CURLOPT_SSL_OPTIONS` option.
    pub fn ssl_options(&mut self, bits: &SslOpt) -> Result<(), Error> {
        self.inner.ssl_options = bits.bits;
        self.setopt_long(curl_sys::CURLOPT_SSL_OPTIONS, bits.bits)
    }

    fn disable_native_ca(&mut self) {
        self.inner.ssl_options &= !(curl_sys::CURLSSLOPT_NATIVE_CA as c_long);
        let _ = self.setopt_long(curl_sys::CURLOPT_SSL_OPTIONS, self.inner.ssl_options);
    }

    /// Set SSL behavior options for proxies
    ///
    /// Inform libcurl about SSL specific behaviors.
    ///
    /// This corresponds to the `CURLOPT_PROXY_SSL_OPTIONS` option.
    pub fn proxy_ssl_options(&mut self, bits: &SslOpt) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_PROXY_SSL_OPTIONS, bits.bits)
    }

    // =========================================================================
    // getters

    /// Set maximum time to wait for Expect 100 request before sending body.
    ///
    /// `curl` has internal heuristics that trigger the use of a `Expect`
    /// header for large enough request bodies where the client first sends the
    /// request header along with an `Expect: 100-continue` header. The server
    /// is supposed to validate the headers and respond with a `100` response
    /// status code after which `curl` will send the actual request body.
    ///
    /// However, if the server does not respond to the initial request
    /// within `CURLOPT_EXPECT_100_TIMEOUT_MS` then `curl` will send the
    /// request body anyways.
    ///
    /// The best-case scenario is where the request is invalid and the server
    /// replies with a `417 Expectation Failed` without having to wait for or process
    /// the request body at all. However, this behaviour can also lead to higher
    /// total latency since in the best case, an additional server roundtrip is required
    /// and in the worst case, the request is delayed by `CURLOPT_EXPECT_100_TIMEOUT_MS`.
    ///
    /// More info: <https://curl.se/libcurl/c/CURLOPT_EXPECT_100_TIMEOUT_MS.html>
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_EXPECT_100_TIMEOUT_MS`.
    pub fn expect_100_timeout(&mut self, timeout: Duration) -> Result<(), Error> {
        let ms = timeout.as_secs() * 1000 + timeout.subsec_millis() as u64;
        self.setopt_long(curl_sys::CURLOPT_EXPECT_100_TIMEOUT_MS, ms as c_long)
    }

    /// Get info on unmet time conditional
    ///
    /// Returns if the condition provided in the previous request didn't match
    ///
    //// This corresponds to `CURLINFO_CONDITION_UNMET` and may return an error if the
    /// option is not supported
    pub fn time_condition_unmet(&self) -> Result<bool, Error> {
        self.getopt_long(curl_sys::CURLINFO_CONDITION_UNMET)
            .map(|r| r != 0)
    }

    /// Get the last used URL
    ///
    /// In cases when you've asked libcurl to follow redirects, it may
    /// not be the same value you set with `url`.
    ///
    /// This methods corresponds to the `CURLINFO_EFFECTIVE_URL` option.
    ///
    /// Returns `Ok(None)` if no effective url is listed or `Err` if an error
    /// happens or the underlying bytes aren't valid utf-8.
    pub fn effective_url(&self) -> Result<Option<&str>, Error> {
        self.getopt_str(curl_sys::CURLINFO_EFFECTIVE_URL)
    }

    /// Get the last used URL, in bytes
    ///
    /// In cases when you've asked libcurl to follow redirects, it may
    /// not be the same value you set with `url`.
    ///
    /// This methods corresponds to the `CURLINFO_EFFECTIVE_URL` option.
    ///
    /// Returns `Ok(None)` if no effective url is listed or `Err` if an error
    /// happens or the underlying bytes aren't valid utf-8.
    pub fn effective_url_bytes(&self) -> Result<Option<&[u8]>, Error> {
        self.getopt_bytes(curl_sys::CURLINFO_EFFECTIVE_URL)
    }

    /// Get the last response code
    ///
    /// The stored value will be zero if no server response code has been
    /// received. Note that a proxy's CONNECT response should be read with
    /// `http_connectcode` and not this.
    ///
    /// Corresponds to `CURLINFO_RESPONSE_CODE` and returns an error if this
    /// option is not supported.
    pub fn response_code(&self) -> Result<u32, Error> {
        self.getopt_long(curl_sys::CURLINFO_RESPONSE_CODE)
            .map(|c| c as u32)
    }

    /// Get the CONNECT response code
    ///
    /// Returns the last received HTTP proxy response code to a CONNECT request.
    /// The returned value will be zero if no such response code was available.
    ///
    /// Corresponds to `CURLINFO_HTTP_CONNECTCODE` and returns an error if this
    /// option is not supported.
    pub fn http_connectcode(&self) -> Result<u32, Error> {
        self.getopt_long(curl_sys::CURLINFO_HTTP_CONNECTCODE)
            .map(|c| c as u32)
    }

    /// Get the remote time of the retrieved document
    ///
    /// Returns the remote time of the retrieved document (in number of seconds
    /// since 1 Jan 1970 in the GMT/UTC time zone). If you get `None`, it can be
    /// because of many reasons (it might be unknown, the server might hide it
    /// or the server doesn't support the command that tells document time etc)
    /// and the time of the document is unknown.
    ///
    /// Note that you must tell the server to collect this information before
    /// the transfer is made, by using the `filetime` method to
    /// or you will unconditionally get a `None` back.
    ///
    /// This corresponds to `CURLINFO_FILETIME` and may return an error if the
    /// option is not supported
    pub fn filetime(&self) -> Result<Option<i64>, Error> {
        self.getopt_long(curl_sys::CURLINFO_FILETIME)
            .map(|r| if r == -1 { None } else { Some(r as i64) })
    }

    /// Get the number of downloaded bytes
    ///
    /// Returns the total amount of bytes that were downloaded.
    /// The amount is only for the latest transfer and will be reset again for each new transfer.
    /// This counts actual payload data, what's also commonly called body.
    /// All meta and header data are excluded and will not be counted in this number.
    ///
    /// This corresponds to `CURLINFO_SIZE_DOWNLOAD` and may return an error if the
    /// option is not supported
    pub fn download_size(&self) -> Result<f64, Error> {
        self.getopt_double(curl_sys::CURLINFO_SIZE_DOWNLOAD)
            .map(|r| r as f64)
    }

    /// Get the number of uploaded bytes
    ///
    /// Returns the total amount of bytes that were uploaded.
    ///
    /// This corresponds to `CURLINFO_SIZE_UPLOAD` and may return an error if the
    /// option is not supported
    pub fn upload_size(&self) -> Result<f64, Error> {
        self.getopt_double(curl_sys::CURLINFO_SIZE_UPLOAD)
            .map(|r| r as f64)
    }

    /// Get the content-length of the download
    ///
    /// Returns the content-length of the download.
    /// This is the value read from the Content-Length: field
    ///
    /// This corresponds to `CURLINFO_CONTENT_LENGTH_DOWNLOAD` and may return an error if the
    /// option is not supported
    pub fn content_length_download(&self) -> Result<f64, Error> {
        self.getopt_double(curl_sys::CURLINFO_CONTENT_LENGTH_DOWNLOAD)
            .map(|r| r as f64)
    }

    /// Get total time of previous transfer
    ///
    /// Returns the total time for the previous transfer,
    /// including name resolving, TCP connect etc.
    ///
    /// Corresponds to `CURLINFO_TOTAL_TIME` and may return an error if the
    /// option isn't supported.
    pub fn total_time(&self) -> Result<Duration, Error> {
        self.getopt_double(curl_sys::CURLINFO_TOTAL_TIME)
            .map(double_seconds_to_duration)
    }

    /// Get the name lookup time
    ///
    /// Returns the total time from the start
    /// until the name resolving was completed.
    ///
    /// Corresponds to `CURLINFO_NAMELOOKUP_TIME` and may return an error if the
    /// option isn't supported.
    pub fn namelookup_time(&self) -> Result<Duration, Error> {
        self.getopt_double(curl_sys::CURLINFO_NAMELOOKUP_TIME)
            .map(double_seconds_to_duration)
    }

    /// Get the time until connect
    ///
    /// Returns the total time from the start
    /// until the connection to the remote host (or proxy) was completed.
    ///
    /// Corresponds to `CURLINFO_CONNECT_TIME` and may return an error if the
    /// option isn't supported.
    pub fn connect_time(&self) -> Result<Duration, Error> {
        self.getopt_double(curl_sys::CURLINFO_CONNECT_TIME)
            .map(double_seconds_to_duration)
    }

    /// Get the time until the SSL/SSH handshake is completed
    ///
    /// Returns the total time it took from the start until the SSL/SSH
    /// connect/handshake to the remote host was completed. This time is most often
    /// very near to the `pretransfer_time` time, except for cases such as
    /// HTTP pipelining where the pretransfer time can be delayed due to waits in
    /// line for the pipeline and more.
    ///
    /// Corresponds to `CURLINFO_APPCONNECT_TIME` and may return an error if the
    /// option isn't supported.
    pub fn appconnect_time(&self) -> Result<Duration, Error> {
        self.getopt_double(curl_sys::CURLINFO_APPCONNECT_TIME)
            .map(double_seconds_to_duration)
    }

    /// Get the time until the file transfer start
    ///
    /// Returns the total time it took from the start until the file
    /// transfer is just about to begin. This includes all pre-transfer commands
    /// and negotiations that are specific to the particular protocol(s) involved.
    /// It does not involve the sending of the protocol- specific request that
    /// triggers a transfer.
    ///
    /// Corresponds to `CURLINFO_PRETRANSFER_TIME` and may return an error if the
    /// option isn't supported.
    pub fn pretransfer_time(&self) -> Result<Duration, Error> {
        self.getopt_double(curl_sys::CURLINFO_PRETRANSFER_TIME)
            .map(double_seconds_to_duration)
    }

    /// Get the time until the first byte is received
    ///
    /// Returns the total time it took from the start until the first
    /// byte is received by libcurl. This includes `pretransfer_time` and
    /// also the time the server needs to calculate the result.
    ///
    /// Corresponds to `CURLINFO_STARTTRANSFER_TIME` and may return an error if the
    /// option isn't supported.
    pub fn starttransfer_time(&self) -> Result<Duration, Error> {
        self.getopt_double(curl_sys::CURLINFO_STARTTRANSFER_TIME)
            .map(double_seconds_to_duration)
    }

    /// Get the time for all redirection steps
    ///
    /// Returns the total time it took for all redirection steps
    /// include name lookup, connect, pretransfer and transfer before final
    /// transaction was started. `redirect_time` contains the complete
    /// execution time for multiple redirections.
    ///
    /// Corresponds to `CURLINFO_REDIRECT_TIME` and may return an error if the
    /// option isn't supported.
    pub fn redirect_time(&self) -> Result<Duration, Error> {
        self.getopt_double(curl_sys::CURLINFO_REDIRECT_TIME)
            .map(double_seconds_to_duration)
    }

    /// Get the number of redirects
    ///
    /// Corresponds to `CURLINFO_REDIRECT_COUNT` and may return an error if the
    /// option isn't supported.
    pub fn redirect_count(&self) -> Result<u32, Error> {
        self.getopt_long(curl_sys::CURLINFO_REDIRECT_COUNT)
            .map(|c| c as u32)
    }

    /// Get the URL a redirect would go to
    ///
    /// Returns the URL a redirect would take you to if you would enable
    /// `follow_location`. This can come very handy if you think using the
    /// built-in libcurl redirect logic isn't good enough for you but you would
    /// still prefer to avoid implementing all the magic of figuring out the new
    /// URL.
    ///
    /// Corresponds to `CURLINFO_REDIRECT_URL` and may return an error if the
    /// url isn't valid utf-8 or an error happens.
    pub fn redirect_url(&self) -> Result<Option<&str>, Error> {
        self.getopt_str(curl_sys::CURLINFO_REDIRECT_URL)
    }

    /// Get the URL a redirect would go to, in bytes
    ///
    /// Returns the URL a redirect would take you to if you would enable
    /// `follow_location`. This can come very handy if you think using the
    /// built-in libcurl redirect logic isn't good enough for you but you would
    /// still prefer to avoid implementing all the magic of figuring out the new
    /// URL.
    ///
    /// Corresponds to `CURLINFO_REDIRECT_URL` and may return an error.
    pub fn redirect_url_bytes(&self) -> Result<Option<&[u8]>, Error> {
        self.getopt_bytes(curl_sys::CURLINFO_REDIRECT_URL)
    }

    /// Get size of retrieved headers
    ///
    /// Corresponds to `CURLINFO_HEADER_SIZE` and may return an error if the
    /// option isn't supported.
    pub fn header_size(&self) -> Result<u64, Error> {
        self.getopt_long(curl_sys::CURLINFO_HEADER_SIZE)
            .map(|c| c as u64)
    }

    /// Get size of sent request.
    ///
    /// Corresponds to `CURLINFO_REQUEST_SIZE` and may return an error if the
    /// option isn't supported.
    pub fn request_size(&self) -> Result<u64, Error> {
        self.getopt_long(curl_sys::CURLINFO_REQUEST_SIZE)
            .map(|c| c as u64)
    }

    /// Get Content-Type
    ///
    /// Returns the content-type of the downloaded object. This is the value
    /// read from the Content-Type: field.  If you get `None`, it means that the
    /// server didn't send a valid Content-Type header or that the protocol
    /// used doesn't support this.
    ///
    /// Corresponds to `CURLINFO_CONTENT_TYPE` and may return an error if the
    /// option isn't supported.
    pub fn content_type(&self) -> Result<Option<&str>, Error> {
        self.getopt_str(curl_sys::CURLINFO_CONTENT_TYPE)
    }

    /// Get Content-Type, in bytes
    ///
    /// Returns the content-type of the downloaded object. This is the value
    /// read from the Content-Type: field.  If you get `None`, it means that the
    /// server didn't send a valid Content-Type header or that the protocol
    /// used doesn't support this.
    ///
    /// Corresponds to `CURLINFO_CONTENT_TYPE` and may return an error if the
    /// option isn't supported.
    pub fn content_type_bytes(&self) -> Result<Option<&[u8]>, Error> {
        self.getopt_bytes(curl_sys::CURLINFO_CONTENT_TYPE)
    }

    /// Get errno number from last connect failure.
    ///
    /// Note that the value is only set on failure, it is not reset upon a
    /// successful operation. The number is OS and system specific.
    ///
    /// Corresponds to `CURLINFO_OS_ERRNO` and may return an error if the
    /// option isn't supported.
    pub fn os_errno(&self) -> Result<i32, Error> {
        self.getopt_long(curl_sys::CURLINFO_OS_ERRNO)
            .map(|c| c as i32)
    }

    /// Get IP address of last connection.
    ///
    /// Returns a string holding the IP address of the most recent connection
    /// done with this curl handle. This string may be IPv6 when that is
    /// enabled.
    ///
    /// Corresponds to `CURLINFO_PRIMARY_IP` and may return an error if the
    /// option isn't supported.
    pub fn primary_ip(&self) -> Result<Option<&str>, Error> {
        self.getopt_str(curl_sys::CURLINFO_PRIMARY_IP)
    }

    /// Get the latest destination port number
    ///
    /// Corresponds to `CURLINFO_PRIMARY_PORT` and may return an error if the
    /// option isn't supported.
    pub fn primary_port(&self) -> Result<u16, Error> {
        self.getopt_long(curl_sys::CURLINFO_PRIMARY_PORT)
            .map(|c| c as u16)
    }

    /// Get local IP address of last connection
    ///
    /// Returns a string holding the IP address of the local end of most recent
    /// connection done with this curl handle. This string may be IPv6 when that
    /// is enabled.
    ///
    /// Corresponds to `CURLINFO_LOCAL_IP` and may return an error if the
    /// option isn't supported.
    pub fn local_ip(&self) -> Result<Option<&str>, Error> {
        self.getopt_str(curl_sys::CURLINFO_LOCAL_IP)
    }

    /// Get the latest local port number
    ///
    /// Corresponds to `CURLINFO_LOCAL_PORT` and may return an error if the
    /// option isn't supported.
    pub fn local_port(&self) -> Result<u16, Error> {
        self.getopt_long(curl_sys::CURLINFO_LOCAL_PORT)
            .map(|c| c as u16)
    }

    // Get number of created connections
    ///
    /// Corresponds to `CURLINFO_NUM_CONNECTS` and may return an error if the
    /// option isn't supported.
    pub fn num_connects(&self) -> Result<u64, Error> {
        self.getopt_long(curl_sys::CURLINFO_NUM_CONNECTS)
            .map(|c| c as u64)
    }

    /// Get all known cookies
    ///
    /// Returns a linked-list of all cookies cURL knows (expired ones, too).
    ///
    /// Corresponds to the `CURLINFO_COOKIELIST` option and may return an error
    /// if the option isn't supported.
    pub fn cookies(&mut self) -> Result<List, Error> {
        unsafe {
            let mut list = ptr::null_mut();
            let rc = curl_sys::curl_easy_getinfo(
                self.inner.handle,
                curl_sys::CURLINFO_COOKIELIST,
                &mut list,
            );
            self.cvt(rc)?;
            Ok(list::from_raw(list))
        }
    }

    /// Wait for pipelining/multiplexing
    ///
    /// Set wait to `true` to tell libcurl to prefer to wait for a connection to
    /// confirm or deny that it can do pipelining or multiplexing before
    /// continuing.
    ///
    /// When about to perform a new transfer that allows pipelining or
    /// multiplexing, libcurl will check for existing connections to re-use and
    /// pipeline on. If no such connection exists it will immediately continue
    /// and create a fresh new connection to use.
    ///
    /// By setting this option to `true` - and having `pipelining(true, true)`
    /// enabled for the multi handle this transfer is associated with - libcurl
    /// will instead wait for the connection to reveal if it is possible to
    /// pipeline/multiplex on before it continues. This enables libcurl to much
    /// better keep the number of connections to a minimum when using pipelining
    /// or multiplexing protocols.
    ///
    /// The effect thus becomes that with this option set, libcurl prefers to
    /// wait and re-use an existing connection for pipelining rather than the
    /// opposite: prefer to open a new connection rather than waiting.
    ///
    /// The waiting time is as long as it takes for the connection to get up and
    /// for libcurl to get the necessary response back that informs it about its
    /// protocol and support level.
    ///
    /// This corresponds to the `CURLOPT_PIPEWAIT` option.
    pub fn pipewait(&mut self, wait: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_PIPEWAIT, wait as c_long)
    }

    /// Allow HTTP/0.9 compliant responses
    ///
    /// Set allow to `true` to tell libcurl to allow HTTP/0.9 responses. A HTTP/0.9
    /// response is a server response entirely without headers and only a body.
    ///
    /// By default this option is not set and corresponds to
    /// `CURLOPT_HTTP09_ALLOWED`.
    pub fn http_09_allowed(&mut self, allow: bool) -> Result<(), Error> {
        self.setopt_long(curl_sys::CURLOPT_HTTP09_ALLOWED, allow as c_long)
    }

    // =========================================================================
    // Other methods

    /// Unpause reading on a connection.
    ///
    /// Using this function, you can explicitly unpause a connection that was
    /// previously paused.
    ///
    /// A connection can be paused by letting the read or the write callbacks
    /// return `ReadError::Pause` or `WriteError::Pause`.
    ///
    /// To unpause, you may for example call this from the progress callback
    /// which gets called at least once per second, even if the connection is
    /// paused.
    ///
    /// The chance is high that you will get your write callback called before
    /// this function returns.
    pub fn unpause_read(&self) -> Result<(), Error> {
        unsafe {
            let rc = curl_sys::curl_easy_pause(self.inner.handle, curl_sys::CURLPAUSE_RECV_CONT);
            self.cvt(rc)
        }
    }

    /// Unpause writing on a connection.
    ///
    /// Using this function, you can explicitly unpause a connection that was
    /// previously paused.
    ///
    /// A connection can be paused by letting the read or the write callbacks
    /// return `ReadError::Pause` or `WriteError::Pause`. A write callback that
    /// returns pause signals to the library that it couldn't take care of any
    /// data at all, and that data will then be delivered again to the callback
    /// when the writing is later unpaused.
    ///
    /// To unpause, you may for example call this from the progress callback
    /// which gets called at least once per second, even if the connection is
    /// paused.
    pub fn unpause_write(&self) -> Result<(), Error> {
        unsafe {
            let rc = curl_sys::curl_easy_pause(self.inner.handle, curl_sys::CURLPAUSE_SEND_CONT);
            self.cvt(rc)
        }
    }

    /// URL encodes a string `s`
    pub fn url_encode(&mut self, s: &[u8]) -> String {
        if s.is_empty() {
            return String::new();
        }
        unsafe {
            let p = curl_sys::curl_easy_escape(
                self.inner.handle,
                s.as_ptr() as *const _,
                s.len() as c_int,
            );
            assert!(!p.is_null());
            let ret = str::from_utf8(CStr::from_ptr(p).to_bytes()).unwrap();
            let ret = String::from(ret);
            curl_sys::curl_free(p as *mut _);
            ret
        }
    }

    /// URL decodes a string `s`, returning `None` if it fails
    pub fn url_decode(&mut self, s: &str) -> Vec<u8> {
        if s.is_empty() {
            return Vec::new();
        }

        // Work around https://curl.haxx.se/docs/adv_20130622.html, a bug where
        // if the last few characters are a bad escape then curl will have a
        // buffer overrun.
        let mut iter = s.chars().rev();
        let orig_len = s.len();
        let mut data;
        let mut s = s;
        if iter.next() == Some('%') || iter.next() == Some('%') || iter.next() == Some('%') {
            data = s.to_string();
            data.push(0u8 as char);
            s = &data[..];
        }
        unsafe {
            let mut len = 0;
            let p = curl_sys::curl_easy_unescape(
                self.inner.handle,
                s.as_ptr() as *const _,
                orig_len as c_int,
                &mut len,
            );
            assert!(!p.is_null());
            let slice = slice::from_raw_parts(p as *const u8, len as usize);
            let ret = slice.to_vec();
            curl_sys::curl_free(p as *mut _);
            ret
        }
    }

    /// Receives data from a connected socket.
    ///
    /// Only useful after a successful `perform` with the `connect_only` option
    /// set as well.
    pub fn recv(&mut self, data: &mut [u8]) -> Result<usize, Error> {
        unsafe {
            let mut n = 0;
            let r = curl_sys::curl_easy_recv(
                self.inner.handle,
                data.as_mut_ptr() as *mut _,
                data.len(),
                &mut n,
            );
            if r == curl_sys::CURLE_OK {
                Ok(n)
            } else {
                Err(Error::new(r))
            }
        }
    }

    /// Sends data over the connected socket.
    ///
    /// Only useful after a successful `perform` with the `connect_only` option
    /// set as well.
    pub fn send(&mut self, data: &[u8]) -> Result<usize, Error> {
        unsafe {
            let mut n = 0;
            let rc = curl_sys::curl_easy_send(
                self.inner.handle,
                data.as_ptr() as *const _,
                data.len(),
                &mut n,
            );
            self.cvt(rc)?;
            Ok(n)
        }
    }

    /// Get a pointer to the raw underlying CURL handle.
    pub fn raw(&self) -> *mut curl_sys::CURL {
        self.inner.handle
    }

    fn setopt_path(&mut self, opt: curl_sys::CURLoption, val: &Path) -> Result<(), Error> {
        use std::os::unix::prelude::*;
        let s = CString::new(val.as_os_str().as_bytes())?;
        self.setopt_str(opt, &s)
    }

    fn setopt_long(&mut self, opt: curl_sys::CURLoption, val: c_long) -> Result<(), Error> {
        unsafe { self.cvt(curl_sys::curl_easy_setopt(self.inner.handle, opt, val)) }
    }

    fn setopt_str(&mut self, opt: curl_sys::CURLoption, val: &CStr) -> Result<(), Error> {
        self.setopt_ptr(opt, val.as_ptr())
    }

    fn setopt_ptr(&self, opt: curl_sys::CURLoption, val: *const c_char) -> Result<(), Error> {
        unsafe { self.cvt(curl_sys::curl_easy_setopt(self.inner.handle, opt, val)) }
    }

    fn setopt_off_t(
        &mut self,
        opt: curl_sys::CURLoption,
        val: curl_sys::curl_off_t,
    ) -> Result<(), Error> {
        unsafe {
            let rc = curl_sys::curl_easy_setopt(self.inner.handle, opt, val);
            self.cvt(rc)
        }
    }

    fn setopt_blob(&mut self, opt: curl_sys::CURLoption, val: &[u8]) -> Result<(), Error> {
        let blob = curl_sys::curl_blob {
            data: val.as_ptr() as *const c_void as *mut c_void,
            len: val.len(),
            flags: curl_sys::CURL_BLOB_COPY,
        };
        let blob_ptr = &blob as *const curl_sys::curl_blob;
        unsafe { self.cvt(curl_sys::curl_easy_setopt(self.inner.handle, opt, blob_ptr)) }
    }

    fn getopt_bytes(&self, opt: curl_sys::CURLINFO) -> Result<Option<&[u8]>, Error> {
        unsafe {
            let p = self.getopt_ptr(opt)?;
            if p.is_null() {
                Ok(None)
            } else {
                Ok(Some(CStr::from_ptr(p).to_bytes()))
            }
        }
    }

    fn getopt_ptr(&self, opt: curl_sys::CURLINFO) -> Result<*const c_char, Error> {
        unsafe {
            let mut p = ptr::null();
            let rc = curl_sys::curl_easy_getinfo(self.inner.handle, opt, &mut p);
            self.cvt(rc)?;
            Ok(p)
        }
    }

    fn getopt_str(&self, opt: curl_sys::CURLINFO) -> Result<Option<&str>, Error> {
        match self.getopt_bytes(opt) {
            Ok(None) => Ok(None),
            Err(e) => Err(e),
            Ok(Some(bytes)) => match str::from_utf8(bytes) {
                Ok(s) => Ok(Some(s)),
                Err(_) => Err(Error::new(curl_sys::CURLE_CONV_FAILED)),
            },
        }
    }

    fn getopt_long(&self, opt: curl_sys::CURLINFO) -> Result<c_long, Error> {
        unsafe {
            let mut p = 0;
            let rc = curl_sys::curl_easy_getinfo(self.inner.handle, opt, &mut p);
            self.cvt(rc)?;
            Ok(p)
        }
    }

    fn getopt_double(&self, opt: curl_sys::CURLINFO) -> Result<c_double, Error> {
        unsafe {
            let mut p = 0 as c_double;
            let rc = curl_sys::curl_easy_getinfo(self.inner.handle, opt, &mut p);
            self.cvt(rc)?;
            Ok(p)
        }
    }

    /// Returns the contents of the internal error buffer, if available.
    ///
    /// When an easy handle is created it configured the `CURLOPT_ERRORBUFFER`
    /// parameter and instructs libcurl to store more error information into a
    /// buffer for better error messages and better debugging. The contents of
    /// that buffer are automatically coupled with all errors for methods on
    /// this type, but if manually invoking APIs the contents will need to be
    /// extracted with this method.
    ///
    /// Put another way, you probably don't need this, you're probably already
    /// getting nice error messages!
    ///
    /// This function will clear the internal buffer, so this is an operation
    /// that mutates the handle internally.
    pub fn take_error_buf(&self) -> Option<String> {
        let mut buf = self.inner.error_buf.borrow_mut();
        if buf[0] == 0 {
            return None;
        }
        let pos = buf.iter().position(|i| *i == 0).unwrap_or(buf.len());
        let msg = String::from_utf8_lossy(&buf[..pos]).into_owned();
        buf[0] = 0;
        Some(msg)
    }

    fn cvt(&self, rc: curl_sys::CURLcode) -> Result<(), Error> {
        if rc == curl_sys::CURLE_OK {
            return Ok(());
        }
        let mut err = Error::new(rc);
        if let Some(msg) = self.take_error_buf() {
            err.set_extra(msg);
        }
        Err(err)
    }
}

impl fmt::Debug for Easy {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Easy")
            .field("handle", &self.inner.handle)
            .field("callbacks", &"owned closures")
            .finish()
    }
}

impl Drop for Easy {
    fn drop(&mut self) {
        unsafe {
            curl_sys::curl_easy_cleanup(self.inner.handle);
        }
    }
}

extern "C" fn header_cb(
    buffer: *mut c_char,
    size: size_t,
    nitems: size_t,
    userptr: *mut c_void,
) -> size_t {
    let keep_going = panic::catch(|| unsafe {
        let data = slice::from_raw_parts(buffer as *const u8, size * nitems);
        (*(userptr as *mut Inner)).callbacks.header(data)
    })
    .unwrap_or(false);
    if keep_going { size * nitems } else { !0 }
}

extern "C" fn write_cb(ptr: *mut c_char, size: size_t, nmemb: size_t, data: *mut c_void) -> size_t {
    panic::catch(|| unsafe {
        let input = slice::from_raw_parts(ptr as *const u8, size * nmemb);
        match (*(data as *mut Inner)).callbacks.write(input) {
            Ok(s) => s,
            Err(WriteError::Pause) => curl_sys::CURL_WRITEFUNC_PAUSE,
        }
    })
    .unwrap_or(!0)
}

extern "C" fn read_cb(ptr: *mut c_char, size: size_t, nmemb: size_t, data: *mut c_void) -> size_t {
    panic::catch(|| unsafe {
        let input = slice::from_raw_parts_mut(ptr as *mut u8, size * nmemb);
        match (*(data as *mut Inner)).callbacks.read(input) {
            Ok(s) => s,
            Err(ReadError::Pause) => curl_sys::CURL_READFUNC_PAUSE,
            Err(ReadError::Abort) => curl_sys::CURL_READFUNC_ABORT,
        }
    })
    .unwrap_or(!0)
}

extern "C" fn seek_cb(data: *mut c_void, offset: curl_sys::curl_off_t, origin: c_int) -> c_int {
    panic::catch(|| unsafe {
        let from = if origin == libc::SEEK_SET {
            SeekFrom::Start(offset as u64)
        } else {
            panic!("unknown origin from libcurl: {}", origin);
        };
        (*(data as *mut Inner)).callbacks.seek(from) as c_int
    })
    .unwrap_or(!0)
}

extern "C" fn progress_cb(
    data: *mut c_void,
    dltotal: c_double,
    dlnow: c_double,
    ultotal: c_double,
    ulnow: c_double,
) -> c_int {
    let keep_going = panic::catch(|| unsafe {
        (*(data as *mut Inner))
            .callbacks
            .progress(dltotal, dlnow, ultotal, ulnow)
    })
    .unwrap_or(false);
    if keep_going { 0 } else { 1 }
}

// TODO: expose `handle`? is that safe?
extern "C" fn debug_cb(
    _handle: *mut curl_sys::CURL,
    kind: curl_sys::curl_infotype,
    data: *mut c_char,
    size: size_t,
    userptr: *mut c_void,
) -> c_int {
    panic::catch(|| unsafe {
        let data = slice::from_raw_parts(data as *const u8, size);
        let kind = match kind {
            curl_sys::CURLINFO_TEXT => InfoType::Text,
            curl_sys::CURLINFO_HEADER_IN => InfoType::HeaderIn,
            curl_sys::CURLINFO_HEADER_OUT => InfoType::HeaderOut,
            curl_sys::CURLINFO_DATA_IN => InfoType::DataIn,
            curl_sys::CURLINFO_DATA_OUT => InfoType::DataOut,
            curl_sys::CURLINFO_SSL_DATA_IN => InfoType::SslDataIn,
            curl_sys::CURLINFO_SSL_DATA_OUT => InfoType::SslDataOut,
            _ => return,
        };
        (*(userptr as *mut Inner)).callbacks.debug(kind, data)
    });
    0
}

// TODO: expose `purpose` and `sockaddr` inside of `address`
extern "C" fn opensocket_cb(
    data: *mut c_void,
    _purpose: curl_sys::curlsocktype,
    address: *mut curl_sys::curl_sockaddr,
) -> curl_sys::curl_socket_t {
    let res = panic::catch(|| unsafe {
        (*(data as *mut Inner))
            .callbacks
            .open_socket((*address).family, (*address).socktype, (*address).protocol)
            .unwrap_or(curl_sys::CURL_SOCKET_BAD)
    });
    res.unwrap_or(curl_sys::CURL_SOCKET_BAD)
}

fn double_seconds_to_duration(seconds: f64) -> Duration {
    let whole_seconds = seconds.trunc() as u64;
    let nanos = seconds.fract() * 1_000_000_000f64;
    Duration::new(whole_seconds, nanos as u32)
}

#[test]
fn double_seconds_to_duration_whole_second() {
    let dur = double_seconds_to_duration(1.0);
    assert_eq!(dur.as_secs(), 1);
    assert_eq!(dur.subsec_nanos(), 0);
}

#[test]
fn double_seconds_to_duration_sub_second1() {
    let dur = double_seconds_to_duration(0.0);
    assert_eq!(dur.as_secs(), 0);
    assert_eq!(dur.subsec_nanos(), 0);
}

#[test]
fn double_seconds_to_duration_sub_second2() {
    let dur = double_seconds_to_duration(0.5);
    assert_eq!(dur.as_secs(), 0);
    assert_eq!(dur.subsec_nanos(), 500_000_000);
}

impl Auth {
    /// Creates a new set of authentications with no members.
    ///
    /// An `Auth` structure is used to configure which forms of authentication
    /// are attempted when negotiating connections with servers.
    pub fn new() -> Auth {
        Auth { bits: 0 }
    }

    /// HTTP Basic authentication.
    ///
    /// This is the default choice, and the only method that is in wide-spread
    /// use and supported virtually everywhere.  This sends the user name and
    /// password over the network in plain text, easily captured by others.
    pub fn basic(&mut self, on: bool) -> &mut Auth {
        self.flag(curl_sys::CURLAUTH_BASIC, on)
    }

    /// HTTP Digest authentication.
    ///
    /// Digest authentication is defined in RFC 2617 and is a more secure way to
    /// do authentication over public networks than the regular old-fashioned
    /// Basic method.
    pub fn digest(&mut self, on: bool) -> &mut Auth {
        self.flag(curl_sys::CURLAUTH_DIGEST, on)
    }

    /// HTTP Digest authentication with an IE flavor.
    ///
    /// Digest authentication is defined in RFC 2617 and is a more secure way to
    /// do authentication over public networks than the regular old-fashioned
    /// Basic method. The IE flavor is simply that libcurl will use a special
    /// "quirk" that IE is known to have used before version 7 and that some
    /// servers require the client to use.
    pub fn digest_ie(&mut self, on: bool) -> &mut Auth {
        self.flag(curl_sys::CURLAUTH_DIGEST_IE, on)
    }

    fn flag(&mut self, bit: c_ulong, on: bool) -> &mut Auth {
        if on {
            self.bits |= bit as c_long;
        } else {
            self.bits &= !bit as c_long;
        }
        self
    }
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let bits = self.bits as c_ulong;
        f.debug_struct("Auth")
            .field("basic", &(bits & curl_sys::CURLAUTH_BASIC != 0))
            .field("digest", &(bits & curl_sys::CURLAUTH_DIGEST != 0))
            .field("digest_ie", &(bits & curl_sys::CURLAUTH_DIGEST_IE != 0))
            .field(
                "gssnegotiate",
                &(bits & curl_sys::CURLAUTH_GSSNEGOTIATE != 0),
            )
            .field("ntlm", &(bits & curl_sys::CURLAUTH_NTLM != 0))
            .field("ntlm_wb", &(bits & curl_sys::CURLAUTH_NTLM_WB != 0))
            .field("aws_sigv4", &(bits & curl_sys::CURLAUTH_AWS_SIGV4 != 0))
            .finish()
    }
}

impl SslOpt {
    /// Creates a new set of SSL options.
    pub fn new() -> SslOpt {
        SslOpt { bits: 0 }
    }

    /// Tell libcurl to automatically locate and use a client certificate for authentication,
    /// when requested by the server.
    ///
    /// This option is only supported for Schannel (the native Windows SSL library).
    /// Prior to 7.77.0 this was the default behavior in libcurl with Schannel.
    ///
    /// Since the server can request any certificate that supports client authentication in
    /// the OS certificate store it could be a privacy violation and unexpected. (Added in 7.77.0)
    pub fn auto_client_cert(&mut self, on: bool) -> &mut SslOpt {
        self.flag(curl_sys::CURLSSLOPT_AUTO_CLIENT_CERT, on)
    }

    /// Tell libcurl to use the operating system's native CA store for certificate verification.
    ///
    /// Works only on Windows when built to use OpenSSL.
    ///
    /// This option is experimental and behavior is subject to change. (Added in 7.71.0)
    pub fn native_ca(&mut self, on: bool) -> &mut SslOpt {
        self.flag(curl_sys::CURLSSLOPT_NATIVE_CA, on)
    }

    /// Tells libcurl to ignore certificate revocation checks in case of missing or
    /// offline distribution points for those SSL backends where such behavior is present.
    ///
    /// This option is only supported for Schannel (the native Windows SSL library).
    ///
    /// If combined with CURLSSLOPT_NO_REVOKE, the latter takes precedence. (Added in 7.70.0)
    pub fn revoke_best_effort(&mut self, on: bool) -> &mut SslOpt {
        self.flag(curl_sys::CURLSSLOPT_REVOKE_BEST_EFFORT, on)
    }

    /// Tells libcurl to not accept "partial" certificate chains, which it otherwise does by default.
    ///
    /// This option is only supported for OpenSSL and will fail the certificate verification
    /// if the chain ends with an intermediate certificate and not with a root cert.
    /// (Added in 7.68.0)
    pub fn no_partial_chain(&mut self, on: bool) -> &mut SslOpt {
        self.flag(curl_sys::CURLSSLOPT_NO_PARTIALCHAIN, on)
    }

    /// Tells libcurl to disable certificate revocation checks for those SSL
    /// backends where such behavior is present.
    ///
    /// Currently this option is only supported for WinSSL (the native Windows
    /// SSL library), with an exception in the case of Windows' Untrusted
    /// Publishers blacklist which it seems can't be bypassed. This option may
    /// have broader support to accommodate other SSL backends in the future.
    /// <https://curl.haxx.se/docs/ssl-compared.html>
    pub fn no_revoke(&mut self, on: bool) -> &mut SslOpt {
        self.flag(curl_sys::CURLSSLOPT_NO_REVOKE, on)
    }

    /// Tells libcurl to not attempt to use any workarounds for a security flaw
    /// in the SSL3 and TLS1.0 protocols.
    ///
    /// If this option isn't used or this bit is set to 0, the SSL layer libcurl
    /// uses may use a work-around for this flaw although it might cause
    /// interoperability problems with some (older) SSL implementations.
    ///
    /// > WARNING: avoiding this work-around lessens the security, and by
    /// > setting this option to 1 you ask for exactly that. This option is only
    /// > supported for DarwinSSL, NSS and OpenSSL.
    pub fn allow_beast(&mut self, on: bool) -> &mut SslOpt {
        self.flag(curl_sys::CURLSSLOPT_ALLOW_BEAST, on)
    }

    fn flag(&mut self, bit: c_long, on: bool) -> &mut SslOpt {
        if on {
            self.bits |= bit as c_long;
        } else {
            self.bits &= !bit as c_long;
        }
        self
    }
}

impl fmt::Debug for SslOpt {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("SslOpt")
            .field(
                "no_revoke",
                &(self.bits & curl_sys::CURLSSLOPT_NO_REVOKE != 0),
            )
            .field(
                "allow_beast",
                &(self.bits & curl_sys::CURLSSLOPT_ALLOW_BEAST != 0),
            )
            .finish()
    }
}

#[derive(Default)]
struct Callbacks {
    write: Option<Box<dyn FnMut(&[u8]) -> Result<usize, WriteError> + 'static>>,
    read: Option<Box<dyn FnMut(&mut [u8]) -> Result<usize, ReadError> + 'static>>,
    seek: Option<Box<dyn FnMut(SeekFrom) -> SeekResult + 'static>>,
    debug: Option<Box<dyn FnMut(InfoType, &[u8]) + 'static>>,
    header: Option<Box<dyn FnMut(&[u8]) -> bool + 'static>>,
    progress: Option<Box<dyn FnMut(f64, f64, f64, f64) -> bool + 'static>>,
    open_socket: Option<Box<dyn FnMut(c_int, c_int, c_int) -> Option<curl_sys::curl_socket_t>>>,
}
impl Callbacks {
    pub(super) fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        match self.write.as_mut() {
            Some(f) => f(data),
            None => Ok(data.len()),
        }
    }
    pub(super) fn read(&mut self, data: &mut [u8]) -> Result<usize, ReadError> {
        match self.read.as_mut() {
            Some(f) => f(data),
            None => Ok(0),
        }
    }
    pub(super) fn seek(&mut self, w: SeekFrom) -> SeekResult {
        match self.seek.as_mut() {
            Some(f) => f(w),
            None => SeekResult::CantSeek,
        }
    }
    pub(super) fn debug(&mut self, k: InfoType, d: &[u8]) {
        match self.debug.as_mut() {
            Some(f) => f(k, d),
            None => debug(k, d),
        }
    }
    pub(super) fn header(&mut self, d: &[u8]) -> bool {
        match self.header.as_mut() {
            Some(f) => f(d),
            None => true,
        }
    }
    pub(super) fn progress(&mut self, a: f64, b: f64, c: f64, d: f64) -> bool {
        match self.progress.as_mut() {
            Some(f) => f(a, b, c, d),
            None => true,
        }
    }
    fn open_socket(
        &mut self,
        family: c_int,
        socktype: c_int,
        protocol: c_int,
    ) -> Option<curl_sys::curl_socket_t> {
        match self.open_socket.as_mut() {
            Some(f) => f(family, socktype, protocol),
            None => open_socket(family, socktype, protocol),
        }
    }
}

impl Easy {
    /// Sets the write callback, which runs on the owning thread.
    pub fn write_function<F>(&mut self, f: F) -> Result<(), Error>
    where
        F: FnMut(&[u8]) -> Result<usize, WriteError> + 'static,
    {
        self.inner.callbacks.write = Some(Box::new(f));
        Ok(())
    }

    /// Sets the read callback, which runs on the owning thread.
    pub fn read_function<F>(&mut self, f: F) -> Result<(), Error>
    where
        F: FnMut(&mut [u8]) -> Result<usize, ReadError> + 'static,
    {
        self.inner.callbacks.read = Some(Box::new(f));
        Ok(())
    }

    /// Sets the seek callback, which runs on the owning thread.
    pub fn seek_function<F>(&mut self, f: F) -> Result<(), Error>
    where
        F: FnMut(SeekFrom) -> SeekResult + 'static,
    {
        self.inner.callbacks.seek = Some(Box::new(f));
        Ok(())
    }

    /// Sets the progress callback, which runs on the owning thread.
    pub fn progress_function<F>(&mut self, f: F) -> Result<(), Error>
    where
        F: FnMut(f64, f64, f64, f64) -> bool + 'static,
    {
        self.inner.callbacks.progress = Some(Box::new(f));
        Ok(())
    }

    /// Sets the debug callback, which runs on the owning thread.
    pub fn debug_function<F>(&mut self, f: F) -> Result<(), Error>
    where
        F: FnMut(InfoType, &[u8]) + 'static,
    {
        self.inner.callbacks.debug = Some(Box::new(f));
        Ok(())
    }

    /// Sets the header callback, which runs on the owning thread.
    pub fn header_function<F>(&mut self, f: F) -> Result<(), Error>
    where
        F: FnMut(&[u8]) -> bool + 'static,
    {
        self.inner.callbacks.header = Some(Box::new(f));
        Ok(())
    }

    /// Overrides socket creation. A returned descriptor transfers ownership to curl.
    pub fn open_socket_function<F>(&mut self, f: F) -> Result<(), Error>
    where
        F: FnMut(c_int, c_int, c_int) -> Option<curl_sys::curl_socket_t> + 'static,
    {
        self.inner.callbacks.open_socket = Some(Box::new(f));
        Ok(())
    }
}
