use super::error::Result;
use crate::model::SerialSettings;

/// What the device sends.
#[async_trait::async_trait]
pub trait SerialReader: Send {
    /// The next bytes the device sent, or `None` once the port is gone (a
    /// USB adapter unplugged). Cancel-safe: the session waits on it and on
    /// other things at once, so dropping a pending call loses no byte.
    async fn read(&mut self) -> Result<Option<Vec<u8>>>;
}

/// What goes to the device.
#[async_trait::async_trait]
pub trait SerialWriter: Send {
    async fn write(&mut self, bytes: &[u8]) -> Result<()>;
    /// A break condition, long enough for a console to see it (magic SysRq,
    /// some boot loaders).
    async fn send_break(&mut self) -> Result<()>;
}

/// An open serial port, split into its two directions.
pub struct SerialLink {
    pub reader: Box<dyn SerialReader>,
    pub writer: Box<dyn SerialWriter>,
}

/// The operator's serial ports.
#[async_trait::async_trait]
pub trait SerialPortAdapter: Send + Sync {
    /// Opens the port, exclusively where the platform allows it, at
    /// `settings`' speed, 8N1, no flow control.
    async fn open(&self, settings: &SerialSettings) -> Result<SerialLink>;
}

#[derive(Clone)]
pub struct SerialPortAdapterService(busybody::Service<Box<dyn SerialPortAdapter>>);

impl SerialPortAdapterService {
    pub fn new<T: SerialPortAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for SerialPortAdapterService {
    type Target = busybody::Service<Box<dyn SerialPortAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
