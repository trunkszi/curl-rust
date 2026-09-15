use curl::{EventLoop, WatchEvents};
use std::time::{Duration, Instant};

#[test]
fn watch_and_unwatch_dispatch() {
    let mut ev = EventLoop::new().unwrap();
    let mut fds = [0; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) },
        0
    );
    let hits = std::rc::Rc::new(std::cell::Cell::new(0));
    let h = hits.clone();
    let token = ev
        .watch(
            fds[0],
            WatchEvents {
                readable: true,
                writable: false,
                error: false,
            },
            move |_| h.set(h.get() + 1),
        )
        .unwrap();
    unsafe {
        libc::write(fds[1], b"x".as_ptr() as _, 1);
    }
    ev.run_once(Duration::from_millis(20)).unwrap();
    assert_eq!(hits.get(), 1);
    ev.unwatch(token).unwrap();
    unsafe {
        libc::write(fds[1], b"y".as_ptr() as _, 1);
    }
    ev.run_once(Duration::from_millis(1)).unwrap();
    assert_eq!(hits.get(), 1);
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}

#[test]
fn timer_updates_timeout_and_fires() {
    let mut ev = EventLoop::new().unwrap();
    let fired = std::rc::Rc::new(std::cell::Cell::new(false));
    let f = fired.clone();
    let at = Instant::now() + Duration::from_millis(5);
    ev.add_timer(at, move || f.set(true));
    assert!(ev.next_timeout().unwrap() <= Duration::from_millis(5));
    ev.run_once(Duration::from_millis(30)).unwrap();
    assert!(fired.get());
}
