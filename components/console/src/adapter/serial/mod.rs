mod error;
mod service;

pub use error::{Error, Result};
pub use service::{
    SerialLink, SerialPortAdapter, SerialPortAdapterService, SerialReader, SerialWriter,
};
