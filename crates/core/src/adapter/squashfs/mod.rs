//! Thin wrapper around the `backhand` crate: the only place in this codebase
//! that knows about backhand's own types. Everything else works in terms of
//! `crate::model::squashfs` types.

pub mod error;
pub mod port;
pub mod service;

pub use error::Error;
pub use port::{Squashfs, SquashfsAdapter};
pub use service::{inspect, write, InspectedEntry};
