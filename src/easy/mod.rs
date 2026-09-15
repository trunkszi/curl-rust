//! Bindings to the "easy" libcurl API.
//!
//! This module contains some simple types like `Easy` and `List` which are just
//! wrappers around the corresponding libcurl types. There's also a few enums
//! scattered about for various options here and there.
//!
//! Most simple usage of libcurl will likely use the `Easy` structure here, and
//! you can find more docs about its usage on that struct.

mod form;
mod handler;
mod list;

pub use self::form::{Form, Part};
pub use self::handler::Easy;
pub use self::handler::{Auth, ProxyType, SslOpt};
pub use self::handler::{HttpVersion, IpResolve, SslVersion, TimeCondition};
pub use self::handler::{InfoType, ReadError, SeekResult, WriteError};
pub use self::list::{Iter, List};
