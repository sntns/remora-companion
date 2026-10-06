//! The station's side of the provisioning protocol (v1): plain HTTP/1.1,
//! JSON, binaries in standard base64. Nothing on it is secret -- a device
//! checks the identity it gets against anchors baked into its image -- so
//! there is no TLS. `serve` projects it onto `StationService`; `client` is
//! the device's side, for `station simulate`.

mod client;
mod error;
mod service;
mod wire;

pub use client::StationClient;
pub use error::{Error, Result};
pub use service::{router, serve};
pub use wire::{
    AckBody, AckState, ClaimBody, ClaimStatusBody, ErrorBody, HardwareBody, HelloBody,
    IdentityBody, ImageBody, LabelBody, StateBody, DEFAULT_PORT, PROTOCOL, SERVICE,
};
