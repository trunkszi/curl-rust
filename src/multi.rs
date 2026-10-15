//! Multi - initiating multiple requests simultaneously

use std::fmt;
use std::marker;
use std::ptr;
use std::rc::Rc;
use std::time::Duration;

use libc::{c_char, c_int, c_long, c_short, c_void};

use libc::{POLLIN, POLLOUT, POLLPRI, pollfd};

use crate::easy::{Easy, List};
use crate::panic;
use crate::{Error, MultiError};

/// A multi handle for initiating multiple connections simultaneously.
///
/// This structure corresponds to `CURLM` in libcurl and provides the ability to
/// have multiple transfers in flight simultaneously. This handle is then used
/// to manage each transfer. The main purpose of a `CURLM` is for the
/// *application* to drive the I/O rather than libcurl itself doing all the
/// blocking. Methods like `action` allow the application to inform libcurl of
/// when events have happened.
///
/// Lots more documentation can be found on the libcurl [multi tutorial] where
/// the APIs correspond pretty closely with this crate.
///
/// [multi tutorial]: https://curl.haxx.se/libcurl/c/libcurl-multi.html
///
/// This value cannot move to another thread:
/// ```compile_fail
/// let value = curl::multi::Multi::new();
/// std::thread::spawn(move || drop(value));
/// ```
/// It cannot be shared across threads through `Arc`:
/// ```compile_fail
/// let value = std::sync::Arc::new(curl::multi::Multi::new());
/// std::thread::spawn(move || drop(value));
/// ```
pub(crate) struct Multi {
    raw: Rc<RawMulti>,
    data: Box<MultiData>,
}

#[derive(Debug)]
struct RawMulti {
    handle: *mut curl_sys::CURLM,
}

struct MultiData {
    socket: Box<dyn FnMut(Socket, SocketEvents, usize)>,
    timer: Box<dyn FnMut(Option<Duration>) -> bool>,
}

/// Message from the `messages` function of a multi handle.
///
/// Currently only indicates whether a transfer is done.
pub struct Message<'multi> {
    ptr: *mut curl_sys::CURLMsg,
    _multi: &'multi Multi,
}

/// Handle owned by a multi session.
pub struct EasyHandle {
    guard: DetachGuard,
    easy: Easy,
    _marker: marker::PhantomData<&'static Multi>,
}

/// A guard struct which guarantees that `curl_multi_remove_handle` will be
/// called on an easy handle, either manually or on drop.
struct DetachGuard {
    multi: Rc<RawMulti>,
    easy: *mut curl_sys::CURL,
}

/// Notification of the events that have happened on a socket.
///
/// This type is passed as an argument to the `action` method on a multi handle
/// to indicate what events have occurred on a socket.
pub struct Events {
    bits: c_int,
}

/// Notification of events that are requested on a socket.
///
/// This type is yielded to the `socket_function` callback to indicate what
/// events are requested on a socket.
pub struct SocketEvents {
    bits: c_int,
}

/// Raw underlying socket type that the multi handles use
pub type Socket = curl_sys::curl_socket_t;

/// File descriptor to wait on for use with the `wait` method on a multi handle.
pub struct WaitFd {
    inner: curl_sys::curl_waitfd,
}

impl Multi {
    /// Creates a new multi session through which multiple HTTP transfers can be
    /// initiated.
    pub fn new() -> Multi {
        unsafe {
            crate::init();
            let ptr = curl_sys::curl_multi_init();
            assert!(!ptr.is_null());
            Multi {
                raw: Rc::new(RawMulti { handle: ptr }),
                data: Box::new(MultiData {
                    socket: Box::new(|_, _, _| ()),
                    timer: Box::new(|_| true),
                }),
            }
        }
    }

    /// Set the callback informed about what to wait for
    ///
    /// When the `action` function runs, it informs the application about
    /// updates in the socket (file descriptor) status by doing none, one, or
    /// multiple calls to the socket callback. The callback gets status updates
    /// with changes since the previous time the callback was called. See
    /// `action` for more details on how the callback is used and should work.
    ///
    /// The `SocketEvents` parameter informs the callback on the status of the
    /// given socket, and the methods on that type can be used to learn about
    /// what's going on with the socket.
    ///
    /// The third `usize` parameter is a custom value set by the `assign` method
    /// below.
    pub fn socket_function<F>(&mut self, f: F) -> Result<(), MultiError>
    where
        F: FnMut(Socket, SocketEvents, usize) + 'static,
    {
        self._socket_function(Box::new(f))
    }

    fn _socket_function(
        &mut self,
        f: Box<dyn FnMut(Socket, SocketEvents, usize)>,
    ) -> Result<(), MultiError> {
        self.data.socket = f;
        let cb: curl_sys::curl_socket_callback = cb;
        self.setopt_ptr(
            curl_sys::CURLMOPT_SOCKETFUNCTION,
            cb as usize as *const c_char,
        )?;
        let ptr = &*self.data as *const _;
        self.setopt_ptr(curl_sys::CURLMOPT_SOCKETDATA, ptr as *const c_char)?;
        return Ok(());

        // TODO: figure out how to expose `_easy`
        extern "C" fn cb(
            _easy: *mut curl_sys::CURL,
            socket: curl_sys::curl_socket_t,
            what: c_int,
            userptr: *mut c_void,
            socketp: *mut c_void,
        ) -> c_int {
            panic::catch(|| unsafe {
                let f = &mut (*(userptr as *mut MultiData)).socket;
                f(socket, SocketEvents { bits: what }, socketp as usize)
            });
            0
        }
    }

    /// Set callback to receive timeout values
    ///
    /// Certain features, such as timeouts and retries, require you to call
    /// libcurl even when there is no activity on the file descriptors.
    ///
    /// Your callback function should install a non-repeating timer with the
    /// interval specified. Each time that timer fires, call either `action` or
    /// `perform`, depending on which interface you use.
    ///
    /// A timeout value of `None` means you should delete your timer.
    ///
    /// A timeout value of 0 means you should call `action` or `perform` (once)
    /// as soon as possible.
    ///
    /// This callback will only be called when the timeout changes.
    ///
    /// The timer callback should return `true` on success, and `false` on
    /// error. This callback can be used instead of, or in addition to,
    /// `get_timeout`.
    pub fn timer_function<F>(&mut self, f: F) -> Result<(), MultiError>
    where
        F: FnMut(Option<Duration>) -> bool + 'static,
    {
        self._timer_function(Box::new(f))
    }

    fn _timer_function(
        &mut self,
        f: Box<dyn FnMut(Option<Duration>) -> bool>,
    ) -> Result<(), MultiError> {
        self.data.timer = f;
        let cb: curl_sys::curl_multi_timer_callback = cb;
        self.setopt_ptr(
            curl_sys::CURLMOPT_TIMERFUNCTION,
            cb as usize as *const c_char,
        )?;
        let ptr = &*self.data as *const _;
        self.setopt_ptr(curl_sys::CURLMOPT_TIMERDATA, ptr as *const c_char)?;
        return Ok(());

        // TODO: figure out how to expose `_multi`
        extern "C" fn cb(
            _multi: *mut curl_sys::CURLM,
            timeout_ms: c_long,
            user: *mut c_void,
        ) -> c_int {
            let keep_going = panic::catch(|| unsafe {
                let f = &mut (*(user as *mut MultiData)).timer;
                if timeout_ms == -1 {
                    f(None)
                } else {
                    f(Some(Duration::from_millis(timeout_ms as u64)))
                }
            })
            .unwrap_or(false);
            if keep_going { 0 } else { -1 }
        }
    }

    fn setopt_ptr(
        &mut self,
        opt: curl_sys::CURLMoption,
        val: *const c_char,
    ) -> Result<(), MultiError> {
        unsafe { cvt(curl_sys::curl_multi_setopt(self.raw.handle, opt, val)) }
    }

    fn setopt_long(&mut self, opt: curl_sys::CURLMoption, val: c_long) -> Result<(), MultiError> {
        unsafe { cvt(curl_sys::curl_multi_setopt(self.raw.handle, opt, val)) }
    }

    /// Sets the maximum number of simultaneously open connections in total (`CURLMOPT_MAX_TOTAL_CONNECTIONS`).
    pub fn max_total_connections(&mut self, max: usize) -> Result<(), MultiError> {
        self.setopt_long(curl_sys::CURLMOPT_MAX_TOTAL_CONNECTIONS, max as c_long)
    }

    /// Sets the maximum number of simultaneously open connections to a single host (`CURLMOPT_MAX_HOST_CONNECTIONS`).
    pub fn max_host_connections(&mut self, max: usize) -> Result<(), MultiError> {
        self.setopt_long(curl_sys::CURLMOPT_MAX_HOST_CONNECTIONS, max as c_long)
    }

    /// Sets the maximum number of connections kept in the connection cache (`CURLMOPT_MAXCONNECTS`).
    pub fn max_connects(&mut self, max: usize) -> Result<(), MultiError> {
        self.setopt_long(curl_sys::CURLMOPT_MAXCONNECTS, max as c_long)
    }

    /// Enables or disables HTTP/1.1 pipelining and HTTP/2 multiplexing (`CURLMOPT_PIPELINING`).
    pub fn pipelining(&mut self, http1: bool, multiplex: bool) -> Result<(), MultiError> {
        let mut bits = curl_sys::CURLPIPE_NOTHING;
        if http1 {
            bits |= curl_sys::CURLPIPE_HTTP1;
        }
        if multiplex {
            bits |= curl_sys::CURLPIPE_MULTIPLEX;
        }
        self.setopt_long(curl_sys::CURLMOPT_PIPELINING, bits)
    }

    /// Add an easy handle to a multi session
    ///
    /// Adds a standard easy handle to the multi stack. This function call will
    /// make this multi handle control the specified easy handle.
    ///
    /// When an easy interface is added to a multi handle, it will use a shared
    /// connection cache owned by the multi handle. Removing and adding new easy
    /// handles will not affect the pool of connections or the ability to do
    /// connection re-use.
    ///
    /// If you have `timer_function` set in the multi handle (and you really
    /// should if you're working event-based with `action` and friends), that
    /// callback will be called from within this function to ask for an updated
    /// timer so that your main event loop will get the activity on this handle
    /// to get started.
    ///
    /// The easy handle will remain added to the multi handle until you remove
    /// it again with `remove` on the returned handle - even when a transfer
    /// with that specific easy handle is completed.
    pub fn add(&self, easy: Easy) -> Result<EasyHandle, (Easy, MultiError)> {
        unsafe {
            if let Err(error) = cvt(curl_sys::curl_multi_add_handle(self.raw.handle, easy.raw())) {
                return Err((easy, error));
            }
        }
        Ok(EasyHandle {
            guard: DetachGuard {
                multi: self.raw.clone(),
                easy: easy.raw(),
            },
            easy,
            _marker: marker::PhantomData,
        })
    }

    /// Remove an easy handle from this multi session
    ///
    /// Removes the easy handle from this multi handle. This will make the
    /// returned easy handle be removed from this multi handle's control.
    ///
    /// When the easy handle has been removed from a multi stack, it is again
    /// perfectly legal to invoke `perform` on it.
    ///
    /// Removing an easy handle while being used is perfectly legal and will
    /// effectively halt the transfer in progress involving that easy handle.
    /// All other easy handles and transfers will remain unaffected.
    pub fn remove(&self, mut easy: EasyHandle) -> Result<Easy, MultiError> {
        easy.guard.detach()?;
        Ok(easy.easy)
    }

    /// Read multi stack informationals
    ///
    /// Ask the multi handle if there are any messages/informationals from the
    /// individual transfers. Messages may include informationals such as an
    /// error code from the transfer or just the fact that a transfer is
    /// completed. More details on these should be written down as well.
    pub fn messages<F>(&self, mut f: F)
    where
        F: FnMut(Message),
    {
        self._messages(&mut f)
    }

    fn _messages(&self, f: &mut dyn FnMut(Message)) {
        let mut queue = 0;
        unsafe {
            loop {
                let ptr = curl_sys::curl_multi_info_read(self.raw.handle, &mut queue);
                if ptr.is_null() {
                    break;
                }
                f(Message { ptr, _multi: self })
            }
        }
    }

    /// Inform of reads/writes available data given an action
    ///
    /// When the application has detected action on a socket handled by libcurl,
    /// it should call this function with the sockfd argument set to
    /// the socket with the action. When the events on a socket are known, they
    /// can be passed `events`. When the events on a socket are unknown, pass
    /// `Events::new()` instead, and libcurl will test the descriptor
    /// internally.
    ///
    /// The returned integer will contain the number of running easy handles
    /// within the multi handle. When this number reaches zero, all transfers
    /// are complete/done. When you call `action` on a specific socket and the
    /// counter decreases by one, it DOES NOT necessarily mean that this exact
    /// socket/transfer is the one that completed. Use `messages` to figure out
    /// which easy handle that completed.
    ///
    /// The `action` function informs the application about updates in the
    /// socket (file descriptor) status by doing none, one, or multiple calls to
    /// the socket callback function set with the `socket_function` method. They
    /// update the status with changes since the previous time the callback was
    /// called.
    pub fn action(&self, socket: Socket, events: &Events) -> Result<u32, MultiError> {
        let mut remaining = 0;
        unsafe {
            cvt(curl_sys::curl_multi_socket_action(
                self.raw.handle,
                socket,
                events.bits,
                &mut remaining,
            ))?;
            Ok(remaining as u32)
        }
    }
}

impl Drop for RawMulti {
    fn drop(&mut self) {
        unsafe {
            let _ = cvt(curl_sys::curl_multi_cleanup(self.handle));
        }
    }
}

fn cvt(code: curl_sys::CURLMcode) -> Result<(), MultiError> {
    if code == curl_sys::CURLM_OK {
        Ok(())
    } else {
        Err(MultiError::new(code))
    }
}

impl fmt::Debug for Multi {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Multi").field("raw", &self.raw).finish()
    }
}

macro_rules! impl_easy_getters {
    () => {
        impl_easy_getters! {
            get {
                time_condition_unmet -> bool,
                effective_url -> Option<&str>,
                effective_url_bytes -> Option<&[u8]>,
                response_code -> u32,
                http_connectcode -> u32,
                filetime -> Option<i64>,
                download_size -> f64,
                upload_size -> f64,
                content_length_download -> f64,
                total_time -> Duration,
                namelookup_time -> Duration,
                connect_time -> Duration,
                appconnect_time -> Duration,
                pretransfer_time -> Duration,
                starttransfer_time -> Duration,
                redirect_time -> Duration,
                redirect_count -> u32,
                redirect_url -> Option<&str>,
                redirect_url_bytes -> Option<&[u8]>,
                header_size -> u64,
                request_size -> u64,
                content_type -> Option<&str>,
                content_type_bytes -> Option<&[u8]>,
                os_errno -> i32,
                primary_ip -> Option<&str>,
                primary_port -> u16,
                local_ip -> Option<&str>,
                local_port -> u16,
                num_connects -> u64,
            }

            get_mut {
                cookies -> List,
            }
        }
    };

    (get { $($name:ident -> $ret:ty,)* } get_mut { $($mname:ident -> $mret:ty,)* }) => {
        $(
            impl_easy_getters!(@ro $name, $ret, concat!(
                "Same as [`Easy::",
                stringify!($name),
                "`](../easy/struct.Easy.html#method.",
                stringify!($name),
                ")."
            ));
        )*
        $(
            impl_easy_getters!(@mut $mname, $mret, concat!(
                "Same as [`Easy::",
                stringify!($mname),
                "`](../easy/struct.Easy.html#method.",
                stringify!($mname),
                ")."
            ));
        )*
    };

    (@ro $name:ident, $ret:ty, $doc:expr) => {
        #[doc = $doc]
        pub fn $name(&self) -> Result<$ret, Error> {
            self.easy.$name()
        }
    };

    (@mut $name:ident, $ret:ty, $doc:expr) => {
        #[doc = $doc]
        pub fn $name(&mut self) -> Result<$ret, Error> {
            self.easy.$name()
        }
    };
}

#[allow(dead_code)]
impl EasyHandle {
    pub(crate) fn easy_mut(&mut self) -> &mut Easy {
        &mut self.easy
    }

    pub(crate) fn raw(&self) -> *mut curl_sys::CURL {
        self.easy.raw()
    }

    /// Sets an internal private token for this `EasyHandle`.
    ///
    /// This function will set the `CURLOPT_PRIVATE` field on the underlying
    /// easy handle.
    pub fn set_token(&mut self, token: usize) -> Result<(), Error> {
        unsafe {
            crate::cvt(curl_sys::curl_easy_setopt(
                self.easy.raw(),
                curl_sys::CURLOPT_PRIVATE,
                token,
            ))
        }
    }

    impl_easy_getters!();

    /// Unpause reading on a connection.
    ///
    /// Using this function, you can explicitly unpause a connection that was
    /// previously paused.
    ///
    /// A connection can be paused by letting the read or the write callbacks
    /// return `ReadError::Pause` or `WriteError::Pause`.
    ///
    /// The chance is high that you will get your write callback called before
    /// this function returns.
    pub fn unpause_read(&self) -> Result<(), Error> {
        self.easy.unpause_read()
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
    pub fn unpause_write(&self) -> Result<(), Error> {
        self.easy.unpause_write()
    }
}

impl fmt::Debug for EasyHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.easy.fmt(f)
    }
}

impl DetachGuard {
    /// Detach the referenced easy handle from its multi handle manually.
    /// Subsequent calls to this method will have no effect.
    fn detach(&mut self) -> Result<(), MultiError> {
        if !self.easy.is_null() {
            unsafe {
                cvt(curl_sys::curl_multi_remove_handle(
                    self.multi.handle,
                    self.easy,
                ))?
            }

            // Set easy to null to signify that the handle was removed.
            self.easy = ptr::null_mut();
        }

        Ok(())
    }
}

impl Drop for DetachGuard {
    fn drop(&mut self) {
        let _ = self.detach();
    }
}

impl<'multi> Message<'multi> {
    /// If this message indicates that a transfer has finished, returns the
    /// result of the transfer in `Some`.
    ///
    /// If the message doesn't indicate that a transfer has finished, then
    /// `None` is returned.
    ///
    /// Note that the `result*_for` methods below should be preferred as they
    /// provide better error messages as the associated error data on the
    /// handle can be associated with the error type.
    pub fn result(&self) -> Option<Result<(), Error>> {
        unsafe {
            if (*self.ptr).msg == curl_sys::CURLMSG_DONE {
                Some(crate::cvt((*self.ptr).data as curl_sys::CURLcode))
            } else {
                None
            }
        }
    }

    /// Returns this completion result only when it belongs to `handle`.
    pub fn result_for(&self, handle: &EasyHandle) -> Option<Result<(), Error>> {
        if !self.is_for(handle) {
            return None;
        }
        let mut result = self.result();
        if let Some(Err(error)) = &mut result {
            if let Some(message) = handle.easy.take_error_buf() {
                error.set_extra(message);
            }
        }
        result
    }

    /// Returns whether this easy message was for the specified easy handle or
    /// not.
    pub fn is_for(&self, handle: &EasyHandle) -> bool {
        unsafe { (*self.ptr).easy_handle == handle.easy.raw() }
    }

    /// Returns the token associated with the easy handle that this message
    /// represents a completion for.
    ///
    /// This function will return the token assigned with
    /// `EasyHandle::set_token`. This reads the `CURLINFO_PRIVATE` field of the
    /// underlying `*mut CURL`.
    pub fn token(&self) -> Result<usize, Error> {
        unsafe {
            let mut p = 0usize;
            crate::cvt(curl_sys::curl_easy_getinfo(
                (*self.ptr).easy_handle,
                curl_sys::CURLINFO_PRIVATE,
                &mut p,
            ))?;
            Ok(p)
        }
    }
}

impl<'a> fmt::Debug for Message<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Message").field("ptr", &self.ptr).finish()
    }
}

impl Events {
    /// Creates a new blank event bit mask.
    pub fn new() -> Events {
        Events { bits: 0 }
    }

    /// Set or unset the whether these events indicate that input is ready.
    pub fn input(&mut self, val: bool) -> &mut Events {
        self.flag(curl_sys::CURL_CSELECT_IN, val)
    }

    /// Set or unset the whether these events indicate that output is ready.
    pub fn output(&mut self, val: bool) -> &mut Events {
        self.flag(curl_sys::CURL_CSELECT_OUT, val)
    }

    /// Set or unset the whether these events indicate that an error has
    /// happened.
    pub fn error(&mut self, val: bool) -> &mut Events {
        self.flag(curl_sys::CURL_CSELECT_ERR, val)
    }

    fn flag(&mut self, flag: c_int, val: bool) -> &mut Events {
        if val {
            self.bits |= flag;
        } else {
            self.bits &= !flag;
        }
        self
    }
}

impl fmt::Debug for Events {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Events")
            .field("input", &(self.bits & curl_sys::CURL_CSELECT_IN != 0))
            .field("output", &(self.bits & curl_sys::CURL_CSELECT_OUT != 0))
            .field("error", &(self.bits & curl_sys::CURL_CSELECT_ERR != 0))
            .finish()
    }
}

impl SocketEvents {
    /// Wait for incoming data. For the socket to become readable.
    pub fn input(&self) -> bool {
        self.bits & curl_sys::CURL_POLL_IN == curl_sys::CURL_POLL_IN
    }

    /// Wait for outgoing data. For the socket to become writable.
    pub fn output(&self) -> bool {
        self.bits & curl_sys::CURL_POLL_OUT == curl_sys::CURL_POLL_OUT
    }

    /// Wait for incoming and outgoing data. For the socket to become readable
    /// or writable.
    pub fn input_and_output(&self) -> bool {
        self.bits & curl_sys::CURL_POLL_INOUT == curl_sys::CURL_POLL_INOUT
    }

    /// The specified socket/file descriptor is no longer used by libcurl.
    pub fn remove(&self) -> bool {
        self.bits & curl_sys::CURL_POLL_REMOVE == curl_sys::CURL_POLL_REMOVE
    }
}

impl fmt::Debug for SocketEvents {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Events")
            .field("input", &self.input())
            .field("output", &self.output())
            .field("remove", &self.remove())
            .finish()
    }
}

#[allow(dead_code)]
impl WaitFd {
    /// Constructs an empty (invalid) WaitFd.
    pub fn new() -> WaitFd {
        WaitFd {
            inner: curl_sys::curl_waitfd {
                fd: 0,
                events: 0,
                revents: 0,
            },
        }
    }

    /// Set the file descriptor to wait for.
    pub fn set_fd(&mut self, fd: Socket) {
        self.inner.fd = fd;
    }

    /// Indicate that the socket should poll on read events such as new data
    /// received.
    ///
    /// Corresponds to `CURL_WAIT_POLLIN`.
    pub fn poll_on_read(&mut self, val: bool) -> &mut WaitFd {
        self.flag(curl_sys::CURL_WAIT_POLLIN, val)
    }

    /// Indicate that the socket should poll on high priority read events such
    /// as out of band data.
    ///
    /// Corresponds to `CURL_WAIT_POLLPRI`.
    pub fn poll_on_priority_read(&mut self, val: bool) -> &mut WaitFd {
        self.flag(curl_sys::CURL_WAIT_POLLPRI, val)
    }

    /// Indicate that the socket should poll on write events such as the socket
    /// being clear to write without blocking.
    ///
    /// Corresponds to `CURL_WAIT_POLLOUT`.
    pub fn poll_on_write(&mut self, val: bool) -> &mut WaitFd {
        self.flag(curl_sys::CURL_WAIT_POLLOUT, val)
    }

    fn flag(&mut self, flag: c_short, val: bool) -> &mut WaitFd {
        if val {
            self.inner.events |= flag;
        } else {
            self.inner.events &= !flag;
        }
        self
    }

    /// After a call to `wait`, returns `true` if `poll_on_read` was set and a
    /// read event occured.
    pub fn received_read(&self) -> bool {
        self.inner.revents & curl_sys::CURL_WAIT_POLLIN == curl_sys::CURL_WAIT_POLLIN
    }

    /// After a call to `wait`, returns `true` if `poll_on_priority_read` was set and a
    /// priority read event occured.
    pub fn received_priority_read(&self) -> bool {
        self.inner.revents & curl_sys::CURL_WAIT_POLLPRI == curl_sys::CURL_WAIT_POLLPRI
    }

    /// After a call to `wait`, returns `true` if `poll_on_write` was set and a
    /// write event occured.
    pub fn received_write(&self) -> bool {
        self.inner.revents & curl_sys::CURL_WAIT_POLLOUT == curl_sys::CURL_WAIT_POLLOUT
    }
}

impl From<pollfd> for WaitFd {
    fn from(pfd: pollfd) -> WaitFd {
        let mut events = 0;
        if pfd.events & POLLIN == POLLIN {
            events |= curl_sys::CURL_WAIT_POLLIN;
        }
        if pfd.events & POLLPRI == POLLPRI {
            events |= curl_sys::CURL_WAIT_POLLPRI;
        }
        if pfd.events & POLLOUT == POLLOUT {
            events |= curl_sys::CURL_WAIT_POLLOUT;
        }
        WaitFd {
            inner: curl_sys::curl_waitfd {
                fd: pfd.fd,
                events,
                revents: 0,
            },
        }
    }
}

impl fmt::Debug for WaitFd {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("WaitFd")
            .field("fd", &self.inner.fd)
            .field("events", &self.inner.fd)
            .field("revents", &self.inner.fd)
            .finish()
    }
}
