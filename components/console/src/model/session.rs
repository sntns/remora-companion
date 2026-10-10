use remora_channel::model::SshRole;
use remora_context::model::ContextOverride;

/// Which serial port, how fast. Always 8N1 without flow control: what
/// every Remora console runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialSettings {
    /// `/dev/ttyUSB0`, `COM3`...
    pub port: String,
    pub baud: u32,
}

/// Open a device's serial console.
pub struct ConsoleRequest {
    pub over: Option<ContextOverride>,
    pub settings: SerialSettings,
    /// The screen's size, rows then columns.
    pub rows: u16,
    pub cols: u16,
    /// Answer login challenges as this role, or leave them to the operator.
    pub login: Option<SshRole>,
}

/// What a console session has to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleEvent {
    /// The device's output changed the screen.
    Screen,
    /// A challenge showed: its code is being asked for.
    LoginRequested {
        device: String,
        account: String,
        role: SshRole,
    },
    /// Its code was typed at the password prompt.
    LoginAnswered {
        device: String,
        account: String,
        role: SshRole,
    },
    /// No code could be had: the operator types one (an offline code).
    LoginFailed { device: String, reason: String },
    /// The serial port went away.
    Closed,
}
