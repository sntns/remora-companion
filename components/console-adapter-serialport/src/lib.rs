//! A real serial port, through tokio-serial. One task owns the port: it
//! reads into a channel (what the reader half drains, cancel-safely) and
//! carries out writes and breaks in order, so a break can't overtake bytes
//! typed before it.

mod service;

pub use service::SerialPortAdapterImpl;
