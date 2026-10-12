#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ext4 backend failed")]
    Ext4,
    #[error("vfat backend failed")]
    Vfat,
    #[error("vfat has no file owners")]
    NoOwners,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
