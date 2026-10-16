use std::{fs::File, io};

/// Everything written to `file` is on its disk: `sync_all`, but for a
/// disk device on macOS, which refuses it (see `macos::device_sync`).
pub fn sync(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if let Some(synced) = crate::macos::device_sync(file) {
        return synced;
    }
    file.sync_all()
}
