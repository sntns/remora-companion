use std::{fs, path::Path};

use walkdir::WalkDir;

use crate::adapter::ext4;

use super::error::Error;

#[derive(Debug)]
pub struct BuildSummary {
    pub entry_count: usize,
    pub bytes_written: u64,
}

/// Build a fresh ext4 image at `output` (created/truncated to `size_bytes`)
/// populated from `source_dir` — the "format + populate" decomposition of
/// `mkfs.ext4 -d CONFIG_DIR` (see `adapter::ext4::format`, which has
/// no populate-at-format option of its own).
pub fn build(
    source_dir: &Path,
    output: &Path,
    size_bytes: u64,
    block_size: u32,
    label: Option<&str>,
) -> Result<BuildSummary, Error> {
    ext4::format(output, size_bytes, block_size, label).map_err(Error::Format)?;

    let mut entry_count = 0usize;
    let mut bytes_written = 0u64;

    for dir_entry in WalkDir::new(source_dir).min_depth(1).sort_by_file_name() {
        let dir_entry = dir_entry.map_err(|e| {
            let path = e
                .path()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| source_dir.to_path_buf());
            Error::Walk {
                path,
                source: std::io::Error::other(e),
            }
        })?;

        let rel_path = dir_entry
            .path()
            .strip_prefix(source_dir)
            .expect("walkdir entries are always under their root");
        let dest_path = to_ext4_path(rel_path);

        let meta = fs::symlink_metadata(dir_entry.path()).map_err(|source| Error::Walk {
            path: dir_entry.path().to_path_buf(),
            source,
        })?;
        let mode = entry_mode(&meta);

        if meta.is_dir() {
            ext4::create_dir(output, 0, size_bytes, &dest_path, mode).map_err(Error::Populate)?;
        } else if meta.is_file() {
            let contents = fs::read(dir_entry.path()).map_err(|source| Error::ReadFile {
                path: dir_entry.path().to_path_buf(),
                source,
            })?;
            bytes_written += contents.len() as u64;
            ext4::write_file(output, 0, size_bytes, &dest_path, &contents, mode)
                .map_err(Error::Populate)?;
        } else {
            return Err(Error::UnsupportedEntry {
                path: dir_entry.path().to_path_buf(),
            });
        }
        entry_count += 1;
    }

    Ok(BuildSummary {
        entry_count,
        bytes_written,
    })
}

/// `rel_path` joined with `/` regardless of host path-separator conventions
/// — ext4 paths are always `/`-separated, even when built on Windows.
fn to_ext4_path(rel_path: &Path) -> String {
    let joined = rel_path
        .iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    format!("/{joined}")
}

#[cfg(unix)]
fn entry_mode(meta: &fs::Metadata) -> u16 {
    use std::os::unix::fs::MetadataExt;
    (meta.mode() & 0o7777) as u16
}

#[cfg(not(unix))]
fn entry_mode(meta: &fs::Metadata) -> u16 {
    if meta.is_dir() {
        0o755
    } else if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, path::PathBuf};

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "remora-etcher-ext4image-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn builds_and_populates_a_config_image() {
        let source = tempdir();
        fs::create_dir_all(source.join("tzdata")).unwrap();
        fs::File::create(source.join("timezone"))
            .unwrap()
            .write_all(b"Europe/Paris\n")
            .unwrap();

        let output = source.join("../config.ext4");
        let summary = build(&source, &output, 8 * 1024 * 1024, 1024, Some("config")).unwrap();

        assert_eq!(summary.entry_count, 2);
        assert!(summary.bytes_written > 0);

        let status = std::process::Command::new("fsck.ext4")
            .args(["-n", "-f"])
            .arg(&output)
            .status()
            .expect("fsck.ext4 not available");
        assert!(status.success());
    }
}
