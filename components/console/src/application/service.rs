use super::error::Result;
use crate::model::{ConsoleEvent, ConsoleRequest};

/// A console being used: the device's screen as its output draws it, and
/// the keyboard's way back.
#[async_trait::async_trait]
pub trait ConsoleSession: Send {
    /// What happened next: the screen changed, a login was answered (or
    /// could not be), or the port went away. Cancel-safe, so it can be
    /// awaited alongside the keyboard.
    async fn next(&mut self) -> Result<ConsoleEvent>;
    /// Sends what was typed, as the bytes a terminal would.
    async fn send(&mut self, bytes: &[u8]) -> Result<()>;
    async fn send_break(&mut self) -> Result<()>;
    /// The screen's new size. A serial line has no way to tell the device:
    /// this is only the model's.
    fn resize(&mut self, rows: u16, cols: u16);
    /// The screen, as the device's output has drawn it so far.
    fn screen(&self) -> &vt100::Screen;
}

/// The console vertical's application-facing port.
#[async_trait::async_trait]
pub trait ConsoleServiceInterface: Send + Sync {
    /// Opens a device's serial console. With `request.login`, a login
    /// challenge the device shows is answered by itself: its code asked of
    /// the platform for that role, and typed at the password prompt.
    async fn open(&self, request: ConsoleRequest) -> Result<Box<dyn ConsoleSession>>;
}

#[derive(Clone)]
pub struct ConsoleService(busybody::Service<Box<dyn ConsoleServiceInterface>>);

impl ConsoleService {
    pub fn new<T: ConsoleServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for ConsoleService {
    type Target = busybody::Service<Box<dyn ConsoleServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
