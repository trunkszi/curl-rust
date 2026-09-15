//! Single-thread socket-action event loop.
mod error;
/// Public interface for this event-loop operation.
pub use error::{AddError, Error};
/// Public interface for this event-loop operation.
pub(crate) mod driver;
use crate::easy::Easy;
use crate::multi::{EasyHandle, Multi};
use crate::reactor::Reactor;
use driver::{Driver, DriverAction, DriverCx, DriverId, Owner};
use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
/// Public event-loop value or operation.
pub struct Token(pub(crate) usize);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
/// Token identifying a registered watcher.
pub struct WatchToken(usize);

#[derive(Copy, Clone, Debug)]
/// Public event-loop value or operation.
pub struct WatchEvents {
    /// Public interface for this event-loop operation.
    pub readable: bool,
    /// Public interface for this event-loop operation.
    pub writable: bool,
    /// Public interface for this event-loop operation.
    pub error: bool,
}

type Completion = Box<dyn FnOnce(Easy, Result<(), Error>)>;
/// Public interface for this event-loop operation.
pub(crate) type DriverFactory = Box<dyn FnOnce(Result<i32, Error>) -> Option<Box<dyn Driver>>>;

struct QueuedRequest {
    token: Token,
    easy: Easy,
    completion: Option<Completion>,
    factory: Option<DriverFactory>,
}

struct ActiveRequest {
    handle: EasyHandle,
    completion: Option<Completion>,
    factory: Option<DriverFactory>,
}

struct DriverEntry {
    handle: EasyHandle,
    fd: i32,
    driver: Box<dyn Driver>,
}

type Watch = (i32, Box<dyn FnMut(WatchEvents)>);
struct Core {
    multi: Multi,
    requests: HashMap<Token, ActiveRequest>,
    drivers: HashMap<DriverId, DriverEntry>,
    watches: HashMap<WatchToken, Watch>,
    timers: BinaryHeap<Reverse<(Instant, usize)>>,
    timer_callbacks: HashMap<usize, Box<dyn FnOnce()>>,
}

/// Public interface for this event-loop operation.
pub(crate) struct LoopInner {
    core: RefCell<Core>,
    queued: RefCell<VecDeque<QueuedRequest>>,
    next_token: Cell<usize>,
    running: Cell<bool>,
    reactor: Rc<RefCell<Reactor>>,
    owners: Rc<RefCell<HashMap<i32, Owner>>>,
    curl_deadline: Rc<Cell<Option<Instant>>>,
    socket_error: Rc<RefCell<Option<std::io::Error>>>,
}

/// A single-thread runner. Client handles borrow it weakly.
pub struct EventLoop {
    /// Public interface for this event-loop operation.
    pub(crate) inner: Rc<LoopInner>,
    _marker: PhantomData<*mut ()>,
}

impl std::fmt::Debug for EventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLoop").finish_non_exhaustive()
    }
}

impl LoopInner {
    fn token(&self) -> Token {
        let id = self.next_token.get();
        self.next_token
            .set(id.checked_add(1).expect("token space exhausted"));
        Token(id)
    }
    /// Public interface for this event-loop operation.
    pub(crate) fn enqueue(
        &self,
        easy: Easy,
        completion: Option<Completion>,
        factory: Option<DriverFactory>,
    ) -> Result<Token, AddError> {
        if !easy.validate_targets() {
            return Err(AddError {
                easy,
                error: Error::UnresolvedHost,
            });
        }
        let token = self.token();
        self.queued.borrow_mut().push_back(QueuedRequest {
            token,
            easy,
            completion,
            factory,
        });
        Ok(token)
    }

    /// Public interface for this event-loop operation.
    pub(crate) fn cancel(&self, token: Token) -> Result<Easy, Error> {
        let queued = {
            let mut q = self.queued.borrow_mut();
            q.iter()
                .position(|r| r.token == token)
                .and_then(|i| q.remove(i))
        };
        if let Some(r) = queued {
            return Ok(r.easy);
        }
        let mut core = self.core.try_borrow_mut().map_err(|_| Error::Reentrant)?;
        if let Some(r) = core.requests.remove(&token) {
            return core.multi.remove(r.handle).map_err(Error::Multi);
        }
        if let Some(entry) = core.drivers.remove(&DriverId(token.0)) {
            self.reactor
                .borrow_mut()
                .deregister(entry.fd)
                .map_err(Error::Io)?;
            // Suppress libcurl's removal callback until multi.remove has completed.
            let result = core.multi.remove(entry.handle).map_err(Error::Multi);
            self.owners.borrow_mut().remove(&entry.fd);
            drop(core);
            let mut driver = entry.driver;
            driver.closed();
            return result;
        }
        Err(Error::Reentrant)
    }

    /// Public interface for this event-loop operation.
    pub(crate) fn with<F, R>(&self, token: Token, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Easy) -> R,
    {
        let queued = {
            let mut q = self.queued.borrow_mut();
            q.iter()
                .position(|r| r.token == token)
                .and_then(|i| q.remove(i))
        };
        if let Some(mut r) = queued {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut r.easy)));
            self.queued.borrow_mut().push_back(r);
            return match result {
                Ok(v) => Ok(v),
                Err(p) => std::panic::resume_unwind(p),
            };
        }
        let mut r = self
            .core
            .try_borrow_mut()
            .map_err(|_| Error::Reentrant)?
            .requests
            .remove(&token)
            .ok_or(Error::Reentrant)?;
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(r.handle.easy_mut())));
        self.core.borrow_mut().requests.insert(token, r);
        match result {
            Ok(v) => Ok(v),
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    fn apply_queued(&self) {
        // A completion can enqueue a follow-up without extending this batch indefinitely.
        let batch = std::mem::take(&mut *self.queued.borrow_mut());
        for request in batch {
            let result = self.core.borrow().multi.add(request.easy);
            match result {
                Ok(handle) => {
                    self.core.borrow_mut().requests.insert(
                        request.token,
                        ActiveRequest {
                            handle,
                            completion: request.completion,
                            factory: request.factory,
                        },
                    );
                }
                Err((easy, error)) => {
                    if let Some(factory) = request.factory {
                        factory(Err(Error::Multi(error)));
                    } else if let Some(cb) = request.completion {
                        cb(easy, Err(Error::Multi(error)));
                    }
                }
            }
        }
    }
    fn socket_check(&self) -> Result<(), Error> {
        if let Some(error) = self.socket_error.borrow_mut().take() {
            return Err(Error::Io(error));
        }
        Ok(())
    }
    fn action(
        &self,
        fd: curl_sys::curl_socket_t,
        events: &crate::multi::Events,
    ) -> Result<(), Error> {
        self.core.borrow().multi.action(fd, events)?;
        crate::panic::propagate();
        self.socket_check()
    }
    fn complete_requests(&self) -> Result<(), Error> {
        let mut done = Vec::new();
        {
            let core = self.core.borrow();
            core.multi.messages(|msg| {
                for (&token, r) in &core.requests {
                    if let Some(result) = msg.result_for(&r.handle) {
                        done.push((token, result));
                        break;
                    }
                }
            });
        }
        crate::panic::propagate();
        for (token, result) in done {
            let Some(mut request) = self.core.borrow_mut().requests.remove(&token) else {
                continue;
            };
            if let Some(factory) = request.factory {
                if let Err(error) = result {
                    self.core.borrow().multi.remove(request.handle)?;
                    factory(Err(Error::Curl(error)));
                    continue;
                }
                let Some(fd) = request.handle.easy_mut().active_socket() else {
                    self.core.borrow().multi.remove(request.handle)?;
                    factory(Err(Error::HandshakeFailed));
                    continue;
                };
                let registration = self
                    .reactor
                    .borrow_mut()
                    .register(fd, fd as usize, true, false);
                if let Err(error) = registration {
                    self.core.borrow().multi.remove(request.handle)?;
                    factory(Err(Error::Io(error)));
                    continue;
                }
                self.owners
                    .borrow_mut()
                    .insert(fd, Owner::Driver(DriverId(token.0)));
                let created =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| factory(Ok(fd))));
                match created {
                    Ok(Some(driver)) => {
                        self.core.borrow_mut().drivers.insert(
                            DriverId(token.0),
                            DriverEntry {
                                handle: request.handle,
                                fd,
                                driver,
                            },
                        );
                        self.drive(DriverId(token.0), Drive::Writable)?;
                    }
                    other => {
                        self.reactor
                            .borrow_mut()
                            .deregister(fd)
                            .map_err(Error::Io)?;
                        self.core.borrow().multi.remove(request.handle)?;
                        self.owners.borrow_mut().remove(&fd);
                        if let Err(panic) = other {
                            std::panic::resume_unwind(panic);
                        }
                    }
                }
            } else {
                let easy = self.core.borrow().multi.remove(request.handle)?;
                if let Some(cb) = request.completion {
                    cb(easy, result.map_err(Error::Curl));
                }
            }
        }
        Ok(())
    }
    fn drive(&self, id: DriverId, event: Drive) -> Result<(), Error> {
        let Some(mut entry) = self.core.borrow_mut().drivers.remove(&id) else {
            return Ok(());
        };
        let curl = entry.handle.easy_mut().raw();
        let result = {
            let mut cx = DriverCx { curl };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match event {
                Drive::Readable => entry.driver.on_readable(&mut cx),
                Drive::Writable => entry.driver.on_writable(&mut cx),
                Drive::Timer(now) => entry.driver.on_timer(now, &mut cx),
            }))
        };
        match result {
            Ok(Ok(DriverAction::Close)) => self.finish(entry),
            Ok(Err(error)) => {
                self.finish(entry)?;
                Err(error)
            }
            Ok(Ok(DriverAction::Continue)) => {
                // Drivers always receive readable events; pending output enables writes.
                let registered = self.reactor.borrow_mut().register(
                    entry.fd,
                    entry.fd as usize,
                    true,
                    entry.driver.wants_write(),
                );
                if let Err(error) = registered {
                    self.finish(entry)?;
                    return Err(Error::Io(error));
                }
                self.core.borrow_mut().drivers.insert(id, entry);
                Ok(())
            }
            Err(panic) => {
                self.core.borrow_mut().drivers.insert(id, entry);
                std::panic::resume_unwind(panic)
            }
        }
    }
    fn finish(&self, mut entry: DriverEntry) -> Result<(), Error> {
        let deregister = self.reactor.borrow_mut().deregister(entry.fd);
        let removed = self.core.borrow().multi.remove(entry.handle);
        self.owners.borrow_mut().remove(&entry.fd);
        entry.driver.closed();
        deregister.map_err(Error::Io)?;
        removed?;
        Ok(())
    }
    fn active(&self) -> bool {
        let core = self.core.borrow();
        !self.queued.borrow().is_empty()
            || !core.requests.is_empty()
            || !core.drivers.is_empty()
            || !core.timer_callbacks.is_empty()
            || !core.watches.is_empty()
    }
    fn next_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        if !self.queued.borrow().is_empty() {
            return Some(Duration::ZERO);
        }
        let core = self.core.borrow();
        let curl = if core.requests.is_empty() {
            None
        } else {
            self.curl_deadline.get()
        };
        let user = core.timers.peek().map(|Reverse((at, _))| *at);
        let driver = core
            .drivers
            .values()
            .filter_map(|d| d.driver.next_deadline())
            .min();
        [curl, user, driver]
            .into_iter()
            .flatten()
            .min()
            .map(|at| at.saturating_duration_since(now))
    }
}
enum Drive {
    Readable,
    Writable,
    Timer(Instant),
}
impl Drop for LoopInner {
    fn drop(&mut self) {
        let core = self.core.get_mut();
        for (_, mut entry) in core.drivers.drain() {
            let _ = self.reactor.borrow_mut().deregister(entry.fd);
            let _ = core.multi.remove(entry.handle);
            self.owners.borrow_mut().remove(&entry.fd);
            entry.driver.closed();
        }
        for (_, r) in core.requests.drain() {
            let _ = core.multi.remove(r.handle);
        }
    }
}
impl EventLoop {
    /// Public interface for this event-loop operation.
    pub fn new() -> Result<Self, Error> {
        let reactor = Rc::new(RefCell::new(Reactor::new().map_err(Error::Io)?));
        let owners = Rc::new(RefCell::new(HashMap::<i32, Owner>::new()));
        let curl_deadline = Rc::new(Cell::new(None));
        let socket_error = Rc::new(RefCell::new(None));
        let mut multi = Multi::new();
        let (r, o, e) = (reactor.clone(), owners.clone(), socket_error.clone());
        multi.socket_function(move |socket, events, _| {
            let fd = socket as i32;
            let owner = o.borrow().get(&fd).copied();
            if matches!(owner, Some(Owner::Driver(_) | Owner::User(_))) {
                return;
            }
            let result = if events.remove() {
                o.borrow_mut().remove(&fd);
                r.borrow_mut().deregister(fd)
            } else {
                o.borrow_mut().insert(fd, Owner::Curl);
                r.borrow_mut().register(
                    fd,
                    fd as usize,
                    events.input() || events.input_and_output(),
                    events.output() || events.input_and_output(),
                )
            };
            if let Err(error) = result {
                *e.borrow_mut() = Some(error);
            }
        })?;
        let deadline = curl_deadline.clone();
        multi.timer_function(move |d| {
            deadline.set(d.map(|d| Instant::now() + d));
            true
        })?;
        Ok(Self {
            inner: Rc::new(LoopInner {
                core: RefCell::new(Core {
                    multi,
                    requests: HashMap::new(),
                    drivers: HashMap::new(),
                    watches: HashMap::new(),
                    timers: BinaryHeap::new(),
                    timer_callbacks: HashMap::new(),
                }),
                queued: RefCell::new(VecDeque::new()),
                next_token: Cell::new(0),
                running: Cell::new(false),
                reactor,
                owners,
                curl_deadline,
                socket_error,
            }),
            _marker: PhantomData,
        })
    }
    /// Public interface for this event-loop operation.
    pub(crate) fn add_driver(&self, easy: Easy, factory: DriverFactory) -> Result<Token, AddError> {
        self.inner.enqueue(easy, None, Some(factory))
    }
    /// Public interface for this event-loop operation.
    pub fn add<F>(&self, easy: Easy, on_done: F) -> Result<Token, AddError>
    where
        F: FnOnce(Easy, Result<(), Error>) + 'static,
    {
        self.inner.enqueue(easy, Some(Box::new(on_done)), None)
    }
    /// Public interface for this event-loop operation.
    pub fn cancel(&self, token: Token) -> Result<Easy, Error> {
        self.inner.cancel(token)
    }
    /// Public interface for this event-loop operation.
    pub fn with<F, R>(&self, token: Token, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Easy) -> R,
    {
        self.inner.with(token, f)
    }
    /// Public interface for this event-loop operation.
    pub fn watch<F>(&self, fd: i32, events: WatchEvents, callback: F) -> Result<WatchToken, Error>
    where
        F: FnMut(WatchEvents) + 'static,
    {
        if self.inner.owners.borrow().contains_key(&fd) {
            return Err(Error::Reentrant);
        }
        let token = WatchToken(self.inner.token().0);
        self.inner
            .reactor
            .borrow_mut()
            .register(fd, fd as usize, events.readable, events.writable)
            .map_err(Error::Io)?;
        self.inner
            .owners
            .borrow_mut()
            .insert(fd, Owner::User(token));
        self.inner
            .core
            .borrow_mut()
            .watches
            .insert(token, (fd, Box::new(callback)));
        Ok(token)
    }
    /// Public interface for this event-loop operation.
    pub fn unwatch(&self, token: WatchToken) -> Result<(), Error> {
        let (fd, _) = self
            .inner
            .core
            .borrow_mut()
            .watches
            .remove(&token)
            .ok_or(Error::Reentrant)?;
        self.inner.owners.borrow_mut().remove(&fd);
        self.inner
            .reactor
            .borrow_mut()
            .deregister(fd)
            .map_err(Error::Io)
    }
    /// Public interface for this event-loop operation.
    pub fn add_timer<F>(&self, at: Instant, callback: F) -> usize
    where
        F: FnOnce() + 'static,
    {
        let id = self.inner.token().0;
        let mut core = self.inner.core.borrow_mut();
        core.timer_callbacks.insert(id, Box::new(callback));
        core.timers.push(Reverse((at, id)));
        id
    }
    /// Public interface for this event-loop operation.
    pub fn run_until<F>(&mut self, mut stop: F) -> Result<(), Error>
    where
        F: FnMut(&Self) -> bool,
    {
        while !stop(self) {
            if !self.run_once(Duration::from_millis(10))? {
                break;
            }
        }
        Ok(())
    }
    /// Public interface for this event-loop operation.
    pub fn run(&mut self) -> Result<(), Error> {
        while self.inner.active() {
            self.run_once(Duration::from_millis(10))?;
        }
        Ok(())
    }
    /// Public interface for this event-loop operation.
    pub fn next_timeout(&self) -> Option<Duration> {
        self.inner.next_timeout()
    }
    /// Public interface for this event-loop operation.
    pub fn as_raw_fd(&self) -> i32 {
        self.inner.reactor.borrow().as_raw_fd()
    }
    /// Public interface for this event-loop operation.
    pub fn run_once(&mut self, wait: Duration) -> Result<bool, Error> {
        if self.inner.running.replace(true) {
            return Err(Error::Reentrant);
        }
        struct Running<'a>(&'a Cell<bool>);
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _running = Running(&self.inner.running);
        self.inner.apply_queued();
        self.inner
            .action(curl_sys::CURL_SOCKET_TIMEOUT, &crate::multi::Events::new())?;
        self.inner.complete_requests()?;
        // Reflect sends queued from the preceding dispatch before waiting.
        let ids: Vec<_> = self.inner.core.borrow().drivers.keys().copied().collect();
        for id in ids {
            let writable = self
                .inner
                .core
                .borrow()
                .drivers
                .get(&id)
                .is_some_and(|d| d.driver.wants_write());
            if writable {
                self.inner.drive(id, Drive::Writable)?;
            }
        }
        let timeout = self.next_timeout().unwrap_or(wait).min(wait);
        let started = Instant::now();
        let mut ready = self
            .inner
            .reactor
            .borrow()
            .wait(timeout)
            .map_err(Error::Io)?;
        // A spurious readiness result must not prevent a pending timer from
        // being observed, but never wait beyond the caller's limit.
        if ready.is_empty() {
            let elapsed = started.elapsed();
            if elapsed < timeout {
                let remaining = timeout - elapsed;
                let extra = self
                    .inner
                    .reactor
                    .borrow()
                    .wait(remaining)
                    .map_err(Error::Io)?;
                ready.extend(extra);
                if ready.is_empty() {
                    // Some reactor backends can report a spurious empty wait;
                    // honor the timer deadline without polling.
                    std::thread::sleep(remaining);
                }
            }
        }
        for event in ready {
            let fd = event.key as i32;
            let owner = self.inner.owners.borrow().get(&fd).copied();
            match owner {
                Some(Owner::Driver(id)) => {
                    if event.readable || event.error {
                        self.inner.drive(id, Drive::Readable)?;
                    }
                    if event.writable {
                        self.inner.drive(id, Drive::Writable)?;
                    }
                }
                Some(Owner::User(token)) => {
                    let watch = self.inner.core.borrow_mut().watches.remove(&token);
                    if let Some((fd, mut callback)) = watch {
                        callback(WatchEvents {
                            readable: event.readable,
                            writable: event.writable,
                            error: event.error,
                        });
                        self.inner
                            .core
                            .borrow_mut()
                            .watches
                            .insert(token, (fd, callback));
                    }
                }
                Some(Owner::Curl) => {
                    let mut events = crate::multi::Events::new();
                    events.input(event.readable);
                    events.output(event.writable);
                    events.error(event.error);
                    self.inner.action(fd as _, &events)?;
                }
                None => {}
            }
        }
        if self
            .inner
            .curl_deadline
            .get()
            .is_some_and(|at| at <= Instant::now())
        {
            self.inner
                .action(curl_sys::CURL_SOCKET_TIMEOUT, &crate::multi::Events::new())?;
        }
        self.inner.complete_requests()?;
        let now = Instant::now();
        let ids: Vec<_> = self.inner.core.borrow().drivers.keys().copied().collect();
        for id in ids {
            self.inner.drive(id, Drive::Timer(now))?;
        }
        loop {
            let callback = {
                let mut core = self.inner.core.borrow_mut();
                match core.timers.peek().copied() {
                    Some(Reverse((at, id))) if at <= Instant::now() => {
                        core.timers.pop();
                        core.timer_callbacks.remove(&id)
                    }
                    _ => None,
                }
            };
            match callback {
                Some(cb) => cb(),
                None => break,
            }
        }
        self.inner.apply_queued();
        Ok(self.inner.active())
    }
}
