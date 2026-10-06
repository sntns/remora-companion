//! The real `FsWalkAdapter`, behind the default `walkdir` feature: a domain
//! crate can name the port and its entry model without depending on it.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use error_stack::{Report, ResultExt};
use walkdir::WalkDir;

use super::error::{Error, Result};
use super::model::{WalkEntry, WalkEntryKind, WalkEntryMetadata};
use super::service::FsWalkAdapter;

#[derive(Debug, Default, Clone, Copy)]
pub struct FsWalkAdapterImpl;

impl FsWalkAdapter for FsWalkAdapterImpl {
    fn walk_dir(&self, root: &Path) -> Result<Vec<WalkEntry>> {
        let mut entries = Vec::new();
        for dir_entry in WalkDir::new(root).min_depth(1) {
            let dir_entry = dir_entry.map_err(|e| {
                let path = e
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| root.to_path_buf());
                Report::new(io::Error::other(e)).change_context(Error::Walk(path))
            })?;
            let rel_path = dir_entry
                .path()
                .strip_prefix(root)
                .expect("walkdir entries are always under their root")
                .to_path_buf();
            entries.push(stat_entry(dir_entry.path(), rel_path)?);
        }
        Ok(entries)
    }
}

fn stat_entry(abs_path: &Path, rel_path: PathBuf) -> Result<WalkEntry> {
    let meta = fs::symlink_metadata(abs_path)
        .change_context_lazy(|| Error::Walk(abs_path.to_path_buf()))?;

    let mtime = mtime_secs(&meta);
    let metadata = entry_metadata(&meta, mtime);

    let kind = if meta.is_dir() {
        WalkEntryKind::Directory
    } else if meta.is_symlink() {
        let target =
            fs::read_link(abs_path).change_context_lazy(|| Error::Walk(abs_path.to_path_buf()))?;
        WalkEntryKind::Symlink { target }
    } else if meta.is_file() {
        WalkEntryKind::File {
            source: abs_path.to_path_buf(),
        }
    } else {
        return Err(Report::new(Error::UnsupportedEntry(abs_path.to_path_buf())));
    };

    Ok(WalkEntry {
        path: rel_path,
        metadata,
        kind,
    })
}

fn mtime_secs(meta: &fs::Metadata) -> u32 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

#[cfg(unix)]
fn entry_metadata(meta: &fs::Metadata, mtime: u32) -> WalkEntryMetadata {
    use std::os::unix::fs::MetadataExt;
    let (uid, gid) = owner(meta);
    WalkEntryMetadata {
        permissions: (meta.mode() & 0o7777) as u16,
        uid,
        gid,
        mtime,
    }
}

#[cfg(not(unix))]
fn entry_metadata(meta: &fs::Metadata, mtime: u32) -> WalkEntryMetadata {
    // mode is approximated from the readonly flag since that's all
    // std::fs::Permissions exposes on non-unix hosts.
    let permissions = if meta.is_dir() {
        0o755
    } else if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    };
    let (uid, gid) = owner(meta);
    WalkEntryMetadata {
        permissions,
        uid,
        gid,
        mtime,
    }
}

#[cfg(unix)]
fn owner(meta: &fs::Metadata) -> (u32, u32) {
    use std::os::unix::fs::MetadataExt;
    (meta.uid(), meta.gid())
}

#[cfg(not(unix))]
fn owner(_meta: &fs::Metadata) -> (u32, u32) {
    // uid/gid have no equivalent on non-unix hosts.
    (0, 0)
}
