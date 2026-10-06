//! The station's side of the provisioning protocol (v1, see
//! `remora-station-protocol`): an axum router projecting it onto
//! `StationService`.

mod error;
mod service;

pub use error::{Error, Result};
pub use service::{router, serve, BODY_LIMIT};
