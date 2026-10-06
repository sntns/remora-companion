//! `StationClientAdapter` over HTTP/1.1 and JSON: the device's side of
//! the protocol `remora-station-protocol` describes.

mod service;

pub use service::HttpStationClientImpl;
