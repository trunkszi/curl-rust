use crate::event_loop::Error;
use std::time::Instant;

pub(crate) struct DriverCx {
    pub curl: *mut curl_sys::CURL,
}

pub(crate) enum DriverAction {
    Continue,
    Close,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct DriverId(pub(crate) usize);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Owner {
    Curl,
    Driver(DriverId),
    User(crate::event_loop::WatchToken),
}

pub(crate) trait Driver {
    fn closed(&mut self) {}
    fn on_readable(&mut self, cx: &mut DriverCx) -> Result<DriverAction, Error>;
    fn on_writable(&mut self, cx: &mut DriverCx) -> Result<DriverAction, Error>;
    fn on_timer(&mut self, now: Instant, cx: &mut DriverCx) -> Result<DriverAction, Error>;
    fn next_deadline(&self) -> Option<Instant>;
    fn wants_write(&self) -> bool;
}
