//! Native readiness reactor used by the single-thread event loop.

use std::io;
use std::time::Duration;
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
}

#[cfg(target_os = "linux")]
impl Reactor {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self { fd })
        }
    }

    pub fn register(
        &mut self,
        socket: i32,
        key: usize,
        readable: bool,
        writable: bool,
    ) -> io::Result<()> {
        let mut events = 0u32;
        if readable {
            events |= libc::EPOLLIN as u32;
        }
        if writable {
            events |= libc::EPOLLOUT as u32;
        }
        events |= libc::EPOLLERR as u32 | libc::EPOLLHUP as u32;
        let mut ev = libc::epoll_event {
            events,
            u64: key as u64,
        };
        let op = libc::EPOLL_CTL_ADD;
        let rc = unsafe { libc::epoll_ctl(self.fd, op, socket, &mut ev) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if op == libc::EPOLL_CTL_ADD && e.raw_os_error() == Some(libc::EEXIST) {
                let rc = unsafe { libc::epoll_ctl(self.fd, libc::EPOLL_CTL_MOD, socket, &mut ev) };
                if rc >= 0 {
                    return Ok(());
                }
            }
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn deregister(&mut self, socket: i32) -> io::Result<()> {
        let rc =
            unsafe { libc::epoll_ctl(self.fd, libc::EPOLL_CTL_DEL, socket, std::ptr::null_mut()) };
        if rc < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn wait(&self, timeout: Duration) -> io::Result<Vec<Ready>> {
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 64];
        let n = unsafe {
            libc::epoll_wait(
                self.fd,
                events.as_mut_ptr(),
                64,
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(events[..n as usize]
                .iter()
                .map(|e| Ready {
                    key: e.u64 as usize,
                    readable: e.events & libc::EPOLLIN as u32 != 0,
                    writable: e.events & libc::EPOLLOUT as u32 != 0,
                    error: e.events & libc::EPOLLERR as u32 != 0,
                })
                .collect())
        }
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
}
#[cfg(target_os = "macos")]
impl Reactor {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self { fd })
        }
    }
    pub fn register(
        &mut self,
        socket: i32,
        key: usize,
        readable: bool,
        writable: bool,
    ) -> io::Result<()> {
        let mut changes = Vec::new();
        for (filter, enabled) in [
            (libc::EVFILT_READ, readable),
            (libc::EVFILT_WRITE, writable),
        ] {
            changes.push(libc::kevent {
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
            });
        }
        let rc = unsafe {
            libc::kevent(
                self.fd,
                changes.as_ptr(),
                changes.len() as i32,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    pub fn deregister(&mut self, socket: i32) -> io::Result<()> {
        self.register(socket, 0, false, false)
    }
    pub fn wait(&self, timeout: Duration) -> io::Result<Vec<Ready>> {
        let ts = libc::timespec {
            tv_sec: timeout.as_secs() as _,
            tv_nsec: timeout.subsec_nanos() as _,
        };
        let mut ev = [libc::kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        }; 64];
        let n = unsafe { libc::kevent(self.fd, std::ptr::null(), 0, ev.as_mut_ptr(), 64, &ts) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        };
        Ok(ev[..n as usize]
            .iter()
            .map(|e| Ready {
                key: e.udata as usize,
                readable: e.filter == libc::EVFILT_READ,
                writable: e.filter == libc::EVFILT_WRITE,
                error: e.flags & libc::EV_ERROR != 0,
            })
            .collect())
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
