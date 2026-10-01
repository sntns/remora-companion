use std::{
    fs,
    path::{Path, PathBuf},
};

use error_stack::{Report, ResultExt};
use remora_fs_walk::FsWalkAdapterService;
use remora_progress::{track_output_file_size, OperationContext, TrackedOutcome};
use remora_squashfs::{
    adapter::{InspectedEntry, SquashfsAdapterService},
    application::{BuildSummary, Error, Result, SquashfsServiceInterface},
    model::{BuildOptions, Entry, EntryKind, EntryMetadata},
};

/// The squashfs vertical's use case: merges `inputs` into one entry list
/// (recursing into directories via the injected `FsWalkAdapterService`,
/// stat-ing lone file inputs directly — see `stat_file_input`), then hands
/// that list to the injected `SquashfsAdapterService` to actually write.
pub struct SquashfsControllerImpl {
    fs_walk: FsWalkAdapterService,
    squashfs: SquashfsAdapterService,
}

impl SquashfsControllerImpl {
    pub fn new(fs_walk: FsWalkAdapterService, squashfs: SquashfsAdapterService) -> Self {
        Self { fs_walk, squashfs }
    }
}

#[async_trait::async_trait]
impl SquashfsServiceInterface for SquashfsControllerImpl {
    async fn build(
        &self,
        inputs: &[PathBuf],
        output: &Path,
        options: &BuildOptions,
        ctx: &OperationContext,
    ) -> Result<BuildSummary> {
        options.validate().change_context(Error::InvalidOptions)?;
        if inputs.is_empty() {
            return Err(Report::new(Error::NoInputs));
        }
        if ctx.cancel.is_cancelled() {
            return Err(Report::new(Error::Cancelled));
        }

        ctx.sink.phase("walking inputs");
        let mut entries = Vec::new();
        let mut newest_mtime: u32 = 0;
        for input in inputs {
            self.collect_entries(input, &mut entries, &mut newest_mtime)?;
        }

        if let Some(pinned) = options.source_date_epoch {
            for entry in &mut entries {
                entry.metadata.mtime = pinned;
            }
        }
        let image_mtime = options.source_date_epoch.unwrap_or(newest_mtime);

        // `mksquashfs` gives the squashfs root inode the real owner of the
        // source directory (not root:root) when there's a single directory
        // input; mirror that rather than hardcoding 0:0, or callers diffing
        // against a real mksquashfs run see a spurious owner mismatch on `/`.
        let root_owner = single_directory_owner(inputs);

        let out_file = fs::File::create(output)
            .map_err(|_| Report::new(Error::CreateOutput(output.to_path_buf())))?;

        // `backhand` buffers pushed entries and does the real read/compress/
        // write work inside this one opaque `write()` call -- track it by
        // polling the output file's size against the summed input size,
        // rather than a per-entry hook that wouldn't reflect real work (see
        // SquashfsServiceInterface::build's doc comment).
        let total_input_bytes: u64 = entries
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::File { source } => fs::metadata(source).ok().map(|m| m.len()),
                _ => None,
            })
            .sum();

        ctx.sink.phase("writing squashfs image");
        let squashfs = self.squashfs.clone();
        let entries_for_work = entries.clone();
        let options_for_work = options.clone();
        let output_for_track = output.to_path_buf();
        match track_output_file_size(ctx, output_for_track, total_input_bytes, move || {
            squashfs.write(
                &entries_for_work,
                &options_for_work,
                image_mtime,
                root_owner,
                out_file,
            )
        })
        .await
        {
            TrackedOutcome::Completed(res) => {
                let bytes_written = res.change_context(Error::Build)?;
                Ok(BuildSummary {
                    entry_count: entries.len(),
                    bytes_written,
                })
            }
            TrackedOutcome::Cancelled => Err(Report::new(Error::Cancelled)),
        }
    }

    async fn inspect(&self, image: &Path) -> Result<Vec<InspectedEntry>> {
        let file = fs::File::open(image)
            .map_err(|_| Report::new(Error::OpenInput(image.to_path_buf())))?;
        self.squashfs.inspect(file).change_context(Error::Build)
    }
}

impl SquashfsControllerImpl {
    fn collect_entries(
        &self,
        input: &Path,
        entries: &mut Vec<Entry>,
        newest_mtime: &mut u32,
    ) -> Result<()> {
        let root_meta = fs::symlink_metadata(input)
            .map_err(|_| Report::new(Error::Walk))
            .attach_with(|| input.display().to_string())?;

        if root_meta.is_dir() {
            let walked = self.fs_walk.walk_dir(input).change_context(Error::Walk)?;
            for entry in walked {
                *newest_mtime = (*newest_mtime).max(entry.metadata.mtime);
                entries.push(entry);
            }
        } else {
            entries.push(stat_file_input(input, newest_mtime)?);
        }

        Ok(())
    }
}

/// A lone file input (as opposed to a directory, which goes through
/// `FsWalkAdapterService`) is placed at the squashfs root under its own
/// basename. One non-recursive `symlink_metadata`/`read_link` call — kept
/// direct rather than routed through a port, same accepted-exception
/// pattern as `remora-flash`'s top-level `File::open` calls.
fn stat_file_input(input: &Path, newest_mtime: &mut u32) -> Result<Entry> {
    let name = input
        .file_name()
        .map(PathBuf::from)
        .ok_or_else(|| Report::new(Error::Walk))?;

    let meta = fs::symlink_metadata(input).map_err(|_| Report::new(Error::Walk))?;
    let mtime = mtime_secs(&meta);
    *newest_mtime = (*newest_mtime).max(mtime);
    let metadata = entry_metadata(&meta, mtime);

    let kind = if meta.is_symlink() {
        let target = fs::read_link(input).map_err(|_| Report::new(Error::Walk))?;
        EntryKind::Symlink { target }
    } else if meta.is_file() {
        EntryKind::File {
            source: input.to_path_buf(),
        }
    } else {
        return Err(Report::new(Error::Walk));
    };

    Ok(Entry {
        path: name,
        metadata,
        kind,
    })
}

/// If `inputs` is a single directory, its real uid/gid — used as the
/// squashfs root inode's owner, matching `mksquashfs`'s own behavior. Falls
/// back to root:root (0:0) for file inputs or multiple inputs, where
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
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
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
    let permissions = if meta.permissions().readonly() {
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
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use remora_fs_walk::FsWalkAdapterImpl;
    use remora_squashfs_adapter_backhand::SquashfsAdapterImpl;
    use std::io::Write;

    #[tokio::test]
    async fn builds_a_minimal_image() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        std::fs::File::create(dir.join("etc/hostname"))
            .unwrap()
            .write_all(b"remora\n")
            .unwrap();

        let controller = SquashfsControllerImpl::new(
            FsWalkAdapterService::new(FsWalkAdapterImpl),
            SquashfsAdapterService::new(SquashfsAdapterImpl),
        );

        let output = dir.join("out.squashfs");
        let summary = controller
            .build(
                std::slice::from_ref(&dir),
                &output,
                &BuildOptions::default(),
                &OperationContext::noop(),
            )
            .await
            .unwrap();

        assert!(summary.entry_count >= 2);
        assert!(output.exists());
        assert!(fs::metadata(&output).unwrap().len() > 0);
    }

    fn tempdir() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "remora-squashfs-application-test-{}-{}",
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
