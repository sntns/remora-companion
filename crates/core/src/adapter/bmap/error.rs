use std::fmt;

#[derive(Debug)]
pub enum Error {
    // bmap-parser's XML error type is not nameable from outside the crate
    // (it's defined in a private submodule), so it's boxed here instead.
    ParseBmap(Box<dyn std::error::Error>),
    Copy(bmap_parser::CopyError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ParseBmap(e) => write!(f, "failed to parse .bmap file: {e}"),
            Error::Copy(e) => write!(f, "failed to copy image: {e}"),
        }
    }
}

impl std::error::Error for Error {}
