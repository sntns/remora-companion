#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid gateway address {0:?}")]
    InvalidAddress(String),
    #[error("invalid TLS configuration")]
    Tls,
    #[error("failed to resolve the gateway's address {0}")]
    Resolve(String),
    #[error("failed to set up the HTTP client")]
    Client,
    #[error("credentials cannot be sent as HTTP headers (they must be printable ASCII)")]
    InvalidCredentials,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

/// Why the platform refused (or failed) a call: its gRPC status code -- the
/// one its REST routes answer in their JSON error body, or the one their
/// HTTP status stands for -- and its own message. Adapters classify it the
/// way they used to classify a gRPC status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub code: Code,
    pub message: String,
}

impl Status {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// No answer from the gateway at all: refused, unreachable, cut off.
    pub fn unavailable(error: &(dyn std::error::Error + 'static)) -> Self {
        Self::new(Code::Unavailable, chain(error))
    }

    /// An answer that is not what the route promises.
    pub(crate) fn malformed(what: impl std::fmt::Display) -> Self {
        Self::new(
            Code::Internal,
            format!("unexpected answer from the gateway: {what}"),
        )
    }
}

/// An error and its causes on one line, as reqwest's and tungstenite's
/// own `Display` leave the interesting part (the OS's words) to `source`.
pub(crate) fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut line = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_line = cause.to_string();
        if !line.contains(&cause_line) {
            line.push_str(": ");
            line.push_str(&cause_line);
        }
        source = cause.source();
    }
    line
}

/// gRPC's status codes, which the platform's REST API keeps: in the
/// `code` of a JSON error body, and behind every HTTP status it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Cancelled,
    Unknown,
    InvalidArgument,
    DeadlineExceeded,
    NotFound,
    AlreadyExists,
    PermissionDenied,
    ResourceExhausted,
    FailedPrecondition,
    Aborted,
    OutOfRange,
    Unimplemented,
    Internal,
    Unavailable,
    DataLoss,
    Unauthenticated,
}

impl Code {
    /// A JSON error body's `code`.
    pub fn from_number(code: i64) -> Self {
        match code {
            1 => Self::Cancelled,
            3 => Self::InvalidArgument,
            4 => Self::DeadlineExceeded,
            5 => Self::NotFound,
            6 => Self::AlreadyExists,
            7 => Self::PermissionDenied,
            8 => Self::ResourceExhausted,
            9 => Self::FailedPrecondition,
            10 => Self::Aborted,
            11 => Self::OutOfRange,
            12 => Self::Unimplemented,
            13 => Self::Internal,
            14 => Self::Unavailable,
            15 => Self::DataLoss,
            16 => Self::Unauthenticated,
            _ => Self::Unknown,
        }
    }

    pub fn number(self) -> i64 {
        match self {
            Self::Cancelled => 1,
            Self::Unknown => 2,
            Self::InvalidArgument => 3,
            Self::DeadlineExceeded => 4,
            Self::NotFound => 5,
            Self::AlreadyExists => 6,
            Self::PermissionDenied => 7,
            Self::ResourceExhausted => 8,
            Self::FailedPrecondition => 9,
            Self::Aborted => 10,
            Self::OutOfRange => 11,
            Self::Unimplemented => 12,
            Self::Internal => 13,
            Self::Unavailable => 14,
            Self::DataLoss => 15,
            Self::Unauthenticated => 16,
        }
    }

    /// What an HTTP status stands for when no JSON body says more, as on
    /// the hand-written routes (artifacts, WebSockets), which answer plain
    /// text. The mapping is grpc-gateway's, reversed: a 400 may also have
    /// been a failed precondition or an out-of-range, which only the
    /// message tells apart.
    pub fn from_http(status: u16) -> Self {
        match status {
            400 => Self::InvalidArgument,
            401 => Self::Unauthenticated,
            403 => Self::PermissionDenied,
            404 => Self::NotFound,
            409 => Self::AlreadyExists,
            412 => Self::FailedPrecondition,
            416 => Self::OutOfRange,
            429 => Self::ResourceExhausted,
            499 => Self::Cancelled,
            501 => Self::Unimplemented,
            502 | 503 => Self::Unavailable,
            504 => Self::DeadlineExceeded,
            500..=599 => Self::Internal,
            _ => Self::Unknown,
        }
    }

    /// The HTTP status grpc-gateway answers for this code.
    pub fn http_status(self) -> u16 {
        match self {
            Self::Cancelled => 499,
            Self::Unknown | Self::Internal | Self::DataLoss => 500,
            Self::InvalidArgument | Self::FailedPrecondition | Self::OutOfRange => 400,
            Self::DeadlineExceeded => 504,
            Self::NotFound => 404,
            Self::AlreadyExists | Self::Aborted => 409,
            Self::PermissionDenied => 403,
            Self::ResourceExhausted => 429,
            Self::Unimplemented => 501,
            Self::Unavailable => 503,
            Self::Unauthenticated => 401,
        }
    }
}

/// One line describing a failed call, for an error attachment: the status
/// code in words and the server's own message, which is where the platform
/// (and, for a channel, the device) explains itself.
pub fn status_summary(status: &Status) -> String {
    if status.message.is_empty() {
        format!("{:?}", status.code)
    } else {
        format!("{:?}: {}", status.code, status.message)
    }
}
