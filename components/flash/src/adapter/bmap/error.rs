#[derive(Debug, thiserror::Error)]
pub enum Error {
    // bmap-parser's XML error type is not nameable from outside the crate
    // (it's defined in a private submodule), so the adapter attaches its
    // message instead of wrapping it.
    #[error("failed to parse .bmap file")]
    ParseBmap,
    #[error("failed to copy image")]
    Copy,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
