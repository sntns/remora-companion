use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error while enumerating disks")]
    Io,
    #[error("no such disk: {0}")]
    NotFound(PathBuf),
    #[error("{0} is not a block device")]
    NotBlockDevice(PathBuf),
    #[error("{0} is not a whole disk (a partition?)")]
    NotWholeDisk(PathBuf),
    #[error("refusing to unmount {0}: it is the system disk")]
    SystemDisk(PathBuf),
    #[error("failed to unmount the volumes of {0}")]
    Unmount(PathBuf),
    #[error("failed to open {0} for writing")]
    Open(PathBuf),
    /// The OS lets only an administrator (root) write the disk, or unmount
    /// its volumes: worth asking for the rights and running again.
    #[error("writing {0} takes administrator rights")]
    PermissionDenied(PathBuf),
    #[error("disk access is not yet implemented on this OS")]
    Unsupported,
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
