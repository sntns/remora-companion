#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ext4 backend failed")]
    Ext4,
    #[error("vfat backend failed")]
    Vfat,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
