use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    NotFound { path: std::path::PathBuf },
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error while enumerating disks: {e}"),
            Error::NotFound { path } => write!(f, "no such disk: {}", path.display()),
            Error::Unsupported => {
                write!(f, "disk enumeration is not yet implemented on this OS")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
