//! Native readiness reactor used by the single-thread event loop.

use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

#[derive(Debug, Copy, Clone)]
pub struct Ready {
    pub key: usize,
    pub readable: bool,
    pub writable: bool,
    pub error: bool,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct Reactor {
    fd: i32,
    interests: HashMap<i32, (usize, bool, bool)>,
}

#[cfg(target_os = "linux")]
impl Reactor {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self {
                fd,
                interests: HashMap::new(),
            })
        }
    }

    #[inline]
    pub fn set_interest(&mut self, socket: i32, readable: bool, writable: bool) -> io::Result<()> {
        self.register(socket, socket as usize, readable, writable)
    }

    pub fn register(
        &mut self,
        socket: i32,
        key: usize,
        readable: bool,
        writable: bool,
    ) -> io::Result<()> {
        let existing = self.interests.get(&socket).copied();
        if existing == Some((key, readable, writable)) {
            return Ok(());
        }

        let mut events = (libc::EPOLLERR | libc::EPOLLHUP) as u32;
        if readable {
            events |= libc::EPOLLIN as u32;
        }
        if writable {
            events |= libc::EPOLLOUT as u32;
        }
        let mut ev = libc::epoll_event {
            events,
            u64: key as u64,
        };

        if existing.is_some() {
            let rc = unsafe { libc::epoll_ctl(self.fd, libc::EPOLL_CTL_MOD, socket, &mut ev) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ENOENT) {
                    let rc_add =
                        unsafe { libc::epoll_ctl(self.fd, libc::EPOLL_CTL_ADD, socket, &mut ev) };
                    if rc_add < 0 {
                        return Err(io::Error::last_os_error());
                    }
                } else {
                    return Err(err);
                }
            }
        } else {
            let rc = unsafe { libc::epoll_ctl(self.fd, libc::EPOLL_CTL_ADD, socket, &mut ev) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EEXIST) {
                    let rc_mod =
                        unsafe { libc::epoll_ctl(self.fd, libc::EPOLL_CTL_MOD, socket, &mut ev) };
                    if rc_mod < 0 {
                        return Err(io::Error::last_os_error());
                    }
                } else {
                    return Err(err);
                }
            }
        }

        self.interests.insert(socket, (key, readable, writable));
        Ok(())
    }

    pub fn deregister(&mut self, socket: i32) -> io::Result<()> {
        let was_registered = self.interests.remove(&socket).is_some();
        let rc =
            unsafe { libc::epoll_ctl(self.fd, libc::EPOLL_CTL_DEL, socket, std::ptr::null_mut()) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if !was_registered
                || matches!(err.raw_os_error(), Some(libc::ENOENT | libc::EBADF))
            {
                return Ok(());
            }
            return Err(err);
        }
        Ok(())
    }

    pub fn wait_into(&self, timeout: Duration, out: &mut Vec<Ready>) -> io::Result<()> {
        out.clear();
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 64];
        let started = Instant::now();
        let mut remaining = timeout;
        let n = loop {
            let timeout_ms = if remaining.is_zero() {
                0
            } else {
                remaining
                    .as_nanos()
                    .div_ceil(1_000_000)
                    .min(i32::MAX as u128) as i32
            };
            let rc = unsafe {
                libc::epoll_wait(
                    self.fd,
                    events.as_mut_ptr(),
                    events.len() as i32,
                    timeout_ms,
                )
            };
            if rc >= 0 {
                break rc as usize;
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    return Ok(());
                }
                remaining = timeout - elapsed;
                continue;
            }
            return Err(err);
        };
        out.reserve(n);
        for e in &events[..n] {
            out.push(Ready {
                key: e.u64 as usize,
                readable: e.events & (libc::EPOLLIN | libc::EPOLLHUP) as u32 != 0,
                writable: e.events & libc::EPOLLOUT as u32 != 0,
                error: e.events & (libc::EPOLLERR | libc::EPOLLHUP) as u32 != 0,
            });
        }
        Ok(())
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.fd
    }
}

#[cfg(target_os = "linux")]
impl Drop for Reactor {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct Reactor {
    fd: i32,
    interests: HashMap<i32, (usize, bool, bool)>,
}

#[cfg(target_os = "macos")]
impl Reactor {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self {
                fd,
                interests: HashMap::new(),
            })
        }
    }

    #[inline]
    pub fn set_interest(&mut self, socket: i32, readable: bool, writable: bool) -> io::Result<()> {
        self.register(socket, socket as usize, readable, writable)
    }

    pub fn register(
        &mut self,
        socket: i32,
        key: usize,
        readable: bool,
        writable: bool,
    ) -> io::Result<()> {
        let existing = self.interests.get(&socket).copied();
        if existing == Some((key, readable, writable)) {
            return Ok(());
        }
        let (prev_key, prev_r, prev_w) = existing.unwrap_or((key, false, false));

        for (filter, prev_enabled, enabled) in [
            (libc::EVFILT_READ, prev_r, readable),
            (libc::EVFILT_WRITE, prev_w, writable),
        ] {
            if enabled != prev_enabled || (enabled && prev_key != key) {
                let change = libc::kevent {
                    ident: socket as libc::uintptr_t,
                    filter,
                    flags: if enabled {
                        libc::EV_ADD | libc::EV_ENABLE
                    } else {
                        libc::EV_DELETE
                    },
                    fflags: 0,
                    data: 0,
                    udata: key as *mut _,
                };
                let rc = unsafe {
                    libc::kevent(
                        self.fd,
                        &change,
                        1,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null(),
                    )
                };
                if rc < 0 {
                    let err = io::Error::last_os_error();
                    if !(!enabled && err.raw_os_error() == Some(libc::ENOENT)) {
                        return Err(err);
                    }
                }
            }
        }

        if readable || writable {
            self.interests.insert(socket, (key, readable, writable));
        } else {
            self.interests.remove(&socket);
        }
        Ok(())
    }

    pub fn deregister(&mut self, socket: i32) -> io::Result<()> {
        let Some((key, prev_r, prev_w)) = self.interests.remove(&socket) else {
            return Ok(());
        };
        let mut changes = [libc::kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        }; 2];
        let mut nchanges = 0usize;
        for (filter, was_enabled) in [
            (libc::EVFILT_READ, prev_r),
            (libc::EVFILT_WRITE, prev_w),
        ] {
            if was_enabled {
                changes[nchanges] = libc::kevent {
                    ident: socket as libc::uintptr_t,
                    filter,
                    flags: libc::EV_DELETE,
                    fflags: 0,
                    data: 0,
                    udata: key as *mut _,
                };
                nchanges += 1;
            }
        }
        if nchanges > 0 {
            let rc = unsafe {
                libc::kevent(
                    self.fd,
                    changes.as_ptr(),
                    nchanges as i32,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if !matches!(err.raw_os_error(), Some(libc::ENOENT | libc::EBADF)) {
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    pub fn wait_into(&self, timeout: Duration, out: &mut Vec<Ready>) -> io::Result<()> {
        out.clear();
        let mut ev = [libc::kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        }; 64];
        let started = Instant::now();
        let mut remaining = timeout;
        let n = loop {
            let ts = libc::timespec {
                tv_sec: remaining.as_secs() as _,
                tv_nsec: remaining.subsec_nanos() as _,
            };
            let rc =
                unsafe { libc::kevent(self.fd, std::ptr::null(), 0, ev.as_mut_ptr(), 64, &ts) };
            if rc >= 0 {
                break rc as usize;
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    return Ok(());
                }
                remaining = timeout - elapsed;
                continue;
            }
            return Err(err);
        };
        out.reserve(n);
        for e in &ev[..n] {
            out.push(Ready {
                key: e.udata as usize,
                readable: e.filter == libc::EVFILT_READ || (e.flags & libc::EV_EOF != 0),
                writable: e.filter == libc::EVFILT_WRITE,
                error: e.flags & libc::EV_ERROR != 0,
            });
        }
        Ok(())
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.fd
    }
}

#[cfg(target_os = "macos")]
impl Drop for Reactor {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_hangup_reports_readable() {
        let mut reactor = Reactor::new().unwrap();
        let mut fds = [0i32; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) },
            0
        );
        reactor.set_interest(fds[0], true, false).unwrap();
        // Repeat identical interest registration (must be no-op)
        reactor.set_interest(fds[0], true, false).unwrap();
        // Modify interest and restore
        reactor.set_interest(fds[0], true, true).unwrap();
        reactor.set_interest(fds[0], true, false).unwrap();

        unsafe {
            libc::close(fds[1]);
        }
        let mut ready = Vec::new();
        reactor
            .wait_into(Duration::from_millis(20), &mut ready)
            .unwrap();
        assert!(!ready.is_empty());
        assert!(ready.iter().any(|r| r.key == fds[0] as usize && r.readable));

        reactor.deregister(fds[0]).unwrap();
        unsafe {
            libc::close(fds[0]);
        }
    }
}

