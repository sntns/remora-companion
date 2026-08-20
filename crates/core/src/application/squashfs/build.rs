use std::{
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use walkdir::WalkDir;

use crate::{
    adapter::squashfs,
    model::squashfs::{BuildOptions, Entry, EntryKind, EntryMetadata},
};

use super::error::Error;

#[derive(Debug)]
pub struct BuildSummary {
    pub entry_count: usize,
    pub bytes_written: u64,
}

/// Build a squashfs image from `inputs` (files and/or directories) into `output`.
///
/// A directory input has its *contents* merged into the squashfs root (like
/// `mksquashfs <dir> out.squashfs`); a file input is placed at the root under
/// its own basename. Real uid/gid/mode from the host filesystem are preserved
/// (no `-all-root` equivalent), matching how `oe_mksquashfs` builds the Remora
/// rootfs.
pub fn build(
    inputs: &[PathBuf],
    output: &Path,
    options: &BuildOptions,
) -> Result<BuildSummary, Error> {
    options.validate()?;
    if inputs.is_empty() {
        return Err(Error::NoInputs);
    }

    let mut entries = Vec::new();
    let mut newest_mtime: u32 = 0;
    for input in inputs {
        collect_entries(input, &mut entries, &mut newest_mtime)?;
    }

    if let Some(pinned) = options.source_date_epoch {
        for entry in &mut entries {
            entry.metadata.mtime = pinned;
        }
    }
    let image_mtime = options.source_date_epoch.unwrap_or(newest_mtime);

    // `mksquashfs` gives the squashfs root inode the real owner of the source
    // directory (not root:root) when there's a single directory input; mirror
    // that rather than hardcoding 0:0, or callers diffing against a real
    // mksquashfs run see a spurious owner mismatch on `/`.
    let root_owner = single_directory_owner(inputs);

    let out_file = fs::File::create(output).map_err(|source| Error::CreateOutput {
        path: output.to_path_buf(),
        source,
    })?;

    let bytes_written = squashfs::write(&entries, options, image_mtime, root_owner, out_file)?;

    Ok(BuildSummary {
        entry_count: entries.len(),
        bytes_written,
    })
}

fn collect_entries(
    input: &Path,
    entries: &mut Vec<Entry>,
    newest_mtime: &mut u32,
) -> Result<(), Error> {
    let root_meta = fs::symlink_metadata(input).map_err(|source| Error::Walk {
        path: input.to_path_buf(),
        source,
    })?;

    if root_meta.is_dir() {
        for dir_entry in WalkDir::new(input).min_depth(1) {
            let dir_entry = dir_entry.map_err(|e| {
                let path = e
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| input.to_path_buf());
                Error::Walk {
                    path,
                    source: io::Error::other(e),
                }
            })?;
            let rel_path = dir_entry
                .path()
                .strip_prefix(input)
                .expect("walkdir entries are always under their root")
                .to_path_buf();
            push_entry(dir_entry.path(), rel_path, entries, newest_mtime)?;
        }
    } else {
        let name = input
            .file_name()
            .map(PathBuf::from)
            .ok_or_else(|| Error::UnsupportedEntry {
                path: input.to_path_buf(),
            })?;
        push_entry(input, name, entries, newest_mtime)?;
    }

    Ok(())
}

fn push_entry(
    abs_path: &Path,
    rel_path: PathBuf,
    entries: &mut Vec<Entry>,
    newest_mtime: &mut u32,
) -> Result<(), Error> {
    let meta = fs::symlink_metadata(abs_path).map_err(|source| Error::Walk {
        path: abs_path.to_path_buf(),
        source,
    })?;

    let mtime = mtime_secs(&meta);
    *newest_mtime = (*newest_mtime).max(mtime);
    let metadata = entry_metadata(&meta, mtime);

    let kind = if meta.is_dir() {
        EntryKind::Directory
    } else if meta.is_symlink() {
        let target = fs::read_link(abs_path).map_err(|source| Error::Walk {
            path: abs_path.to_path_buf(),
            source,
        })?;
        EntryKind::Symlink { target }
    } else if meta.is_file() {
        EntryKind::File {
            source: abs_path.to_path_buf(),
        }
    } else {
        return Err(Error::UnsupportedEntry {
            path: abs_path.to_path_buf(),
        });
    };

    entries.push(Entry {
        path: rel_path,
        metadata,
        kind,
    });
    Ok(())
}

/// If `inputs` is a single directory, its real uid/gid — used as the
/// squashfs root inode's owner, matching `mksquashfs`'s own behavior.
/// Falls back to root:root (0:0) for file inputs or multiple inputs, where
/// there's no single unambiguous source directory to take ownership from.
fn single_directory_owner(inputs: &[PathBuf]) -> (u32, u32) {
    if let [only] = inputs {
        if let Ok(meta) = fs::symlink_metadata(only) {
            if meta.is_dir() {
                return owner(&meta);
            }
        }
    }
    (0, 0)
}

fn mtime_secs(meta: &fs::Metadata) -> u32 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

#[cfg(unix)]
fn entry_metadata(meta: &fs::Metadata, mtime: u32) -> EntryMetadata {
    use std::os::unix::fs::MetadataExt;
    let (uid, gid) = owner(meta);
    EntryMetadata {
        permissions: (meta.mode() & 0o7777) as u16,
        uid,
        gid,
        mtime,
    }
}

#[cfg(not(unix))]
fn entry_metadata(meta: &fs::Metadata, mtime: u32) -> EntryMetadata {
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
    EntryMetadata {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn builds_a_minimal_image() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        std::fs::File::create(dir.join("etc/hostname"))
            .unwrap()
            .write_all(b"remora\n")
            .unwrap();

        let output = dir.join("out.squashfs");
        let summary = build(
            std::slice::from_ref(&dir),
            &output,
            &BuildOptions::default(),
        )
        .unwrap();

        assert!(summary.entry_count >= 2);
        assert!(output.exists());
        assert!(fs::metadata(&output).unwrap().len() > 0);
    }

    fn tempdir() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "remora-etcher-test-{}-{}",
            std::process::id(),
            entry_metadata_test_counter()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry_metadata_test_counter() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }
}
