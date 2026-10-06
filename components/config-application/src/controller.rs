use std::{fs, path::Path};

use error_stack::{Report, ResultExt};
use remora_config::application::{BuildSummary, ConfigServiceInterface, Error, Result};
use remora_fs_walk::{FsWalkAdapterService, WalkEntry, WalkEntryKind};
use remora_image::{application::ImageService, model::PartitionRole};
use remora_scratch::ScratchDir;

/// Default size/block-size for a freshly-built `config.ext4`, matching
/// `remora-config.bbclass`'s own `dd if=/dev/zero ... bs=8M count=1`.
const DEFAULT_CONFIG_SIZE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_CONFIG_BLOCK_SIZE: u32 = 1024;

/// The config vertical's use case: `build` walks a host directory (via the
/// injected `FsWalkAdapterService`) and populates a fresh standalone ext4
/// image with it, and `upload` composes `build` with reading/writing
/// `config.ext4` as a regular file living on the image's shared partition.
/// Every ext4 operation, standalone or in a partition, goes through the
/// injected `ImageService` (the config vertical's cross-vertical
/// dependency).
pub struct ConfigControllerImpl {
    fs_walk: FsWalkAdapterService,
    image: ImageService,
}

impl ConfigControllerImpl {
    pub fn new(fs_walk: FsWalkAdapterService, image: ImageService) -> Self {
        Self { fs_walk, image }
    }

    /// `upload` only ever guarantees the config image's own root exists (a
    /// fresh one is seeded with just `tzdata/`) -- a caller uploading to a
    /// nested path like `/c/coder1.json` needs `/c` created first, same
    /// "narrow surface" rule `ext4_file_write`'s own doc comment states.
    /// Create whatever ancestor directories of `dest_relative_path` don't
    /// exist yet (mkdir -p semantics), so `upload` itself doesn't inherit
    /// that restriction for callers that don't need it.
    async fn ensure_parent_dirs(&self, local_image: &Path, dest_relative_path: &str) -> Result<()> {
        let mut components: Vec<&str> = dest_relative_path
            .split('/')
            .filter(|c| !c.is_empty())
            .collect();
        components.pop(); // drop the file name itself, only ancestors matter here

        let mut current = String::new();
        for component in components {
            current.push('/');
            current.push_str(component);
            let exists = self
                .image
                .ext4_file_exists(local_image, &current)
                .await
                .change_context_lazy(|| Error::Populate(local_image.to_path_buf()))?;
            if !exists {
                self.image
                    .ext4_file_mkdir(local_image, &current, 0o755)
                    .await
                    .change_context_lazy(|| Error::Populate(local_image.to_path_buf()))?;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl ConfigServiceInterface for ConfigControllerImpl {
    async fn build(
        &self,
        source_dir: &Path,
        output: &Path,
        size_bytes: u64,
        block_size: u32,
        label: Option<&str>,
    ) -> Result<BuildSummary> {
        self.image
            .ext4_file_format(output, size_bytes, block_size, label)
            .await
            .change_context_lazy(|| Error::Format(output.to_path_buf()))?;

        let fs_walk = self.fs_walk.clone();
        let source = source_dir.to_path_buf();
        let tree = blocking(move || read_tree(&fs_walk, &source)).await?;

        let mut entry_count = 0usize;
        let mut bytes_written = 0u64;
        for (entry, contents) in tree {
            let dest_path = to_ext4_path(&entry.path);
            let mode = entry.metadata.permissions;
            let populated = match contents {
                None => self.image.ext4_file_mkdir(output, &dest_path, mode).await,
                Some(contents) => {
                    bytes_written += contents.len() as u64;
                    self.image
                        .ext4_file_write(output, &dest_path, &contents, mode)
                        .await
                }
            };
            populated.change_context_lazy(|| Error::Populate(output.to_path_buf()))?;
            entry_count += 1;
        }

        Ok(BuildSummary {
            entry_count,
            bytes_written,
        })
    }

    async fn upload(
        &self,
        image: &Path,
        source: &Path,
        dest_relative_path: &str,
        slot: &str,
        mode: u16,
    ) -> Result<()> {
        let config_path = format!("/remora/{slot}/config");

        let source = source.to_path_buf();
        let contents = blocking(move || {
            fs::read(&source).change_context_lazy(|| Error::ReadSource(source.clone()))
        })
        .await?;

        // `/remora`/`/remora/<slot>` are created at Yocto build time by
        // remora-identity.bbclass/remora-config.bbclass on a real image, but
        // ensure they exist here too rather than assuming it.
        self.image
            .ensure_dir_by_role(image, PartitionRole::Shared, "/remora", 0o755)
            .await
            .change_context(Error::Image)?;
        self.image
            .ensure_dir_by_role(
                image,
                PartitionRole::Shared,
                &format!("/remora/{slot}"),
                0o755,
            )
            .await
            .change_context(Error::Image)?;

        // Removed, with the local copy of config.ext4 in it, however this
        // returns.
        let scratch = ScratchDir::new("remora-config").change_context(Error::Scratch)?;
        let local_image = scratch.join("config.ext4");
        let config_exists = self
            .image
            .exists_by_role(image, PartitionRole::Shared, &config_path)
            .await
            .change_context(Error::Image)?;

        if config_exists {
            let existing = self
                .image
                .read_file_by_role(image, PartitionRole::Shared, &config_path)
                .await
                .change_context(Error::Image)?;
            let local = local_image.clone();
            blocking(move || {
                fs::write(&local, &existing).change_context_lazy(|| Error::WriteTempImage(local))
            })
            .await?;
        } else {
            let seed_dir = scratch.join("seed");
            fs::create_dir_all(seed_dir.join("tzdata"))
                .change_context_lazy(|| Error::WriteTempImage(seed_dir.clone()))?;
            self.build(
                &seed_dir,
                &local_image,
                DEFAULT_CONFIG_SIZE_BYTES,
                DEFAULT_CONFIG_BLOCK_SIZE,
                Some("config"),
            )
            .await?;
        }

        self.ensure_parent_dirs(&local_image, dest_relative_path)
            .await?;
        self.image
            .ext4_file_write(&local_image, dest_relative_path, &contents, mode)
            .await
            .change_context_lazy(|| Error::Populate(local_image.clone()))?;

        let local = local_image.clone();
        let updated =
            blocking(move || fs::read(&local).change_context_lazy(|| Error::ReadTempImage(local)))
                .await?;
        self.image
            .inject_by_role(image, PartitionRole::Shared, &config_path, &updated, 0o644)
            .await
            .change_context(Error::Image)
    }
}

/// Run `op` on the blocking pool, off the runtime threads.
async fn blocking<T: Send + 'static>(op: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(op)
        .await
        .expect("config worker panicked")
}

/// Every entry under `root`, with its content for a file (`None` for a
/// directory). Symlinks are refused.
fn read_tree(
    fs_walk: &FsWalkAdapterService,
    root: &Path,
) -> Result<Vec<(WalkEntry, Option<Vec<u8>>)>> {
    let walked = fs_walk
        .walk_dir(root)
        .change_context_lazy(|| Error::Walk(root.to_path_buf()))?;
    walked
        .into_iter()
        .map(|entry| {
            let contents = match &entry.kind {
                WalkEntryKind::Directory => None,
                WalkEntryKind::File { source } => {
                    Some(fs::read(source).change_context_lazy(|| Error::ReadFile(source.clone()))?)
                }
                WalkEntryKind::Symlink { .. } => {
                    return Err(Report::new(Error::UnsupportedEntry(entry.path)));
                }
            };
            Ok((entry, contents))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn controller() -> ConfigControllerImpl {
        let image = ImageService::new(remora_image_application::ImageControllerImpl::new(
            remora_image::adapter::partition_table::PartitionTableAdapterService::new(
                remora_image_adapter_partition_table::PartitionTableAdapterImpl,
            ),
            Arc::new(remora_image_adapter_ext4::Ext4AdapterImpl),
            Arc::new(remora_image_adapter_vfat::VfatAdapterImpl),
            FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
            remora_image::adapter::ext4::Ext4AdapterService::new(
                remora_image_adapter_ext4::Ext4AdapterImpl,
            ),
        ));
        ConfigControllerImpl::new(
            FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
            image,
        )
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "remora-config-application-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "requires fsck.ext4, a Linux-only dev tool"
    )]
    async fn builds_and_populates_a_config_image() {
        let source = tempdir();
        fs::create_dir_all(source.join("tzdata")).unwrap();
        fs::File::create(source.join("timezone"))
            .unwrap()
            .write_all(b"Europe/Paris\n")
            .unwrap();

        let output = source.join("../config.ext4");
        let summary = controller()
            .build(&source, &output, 8 * 1024 * 1024, 1024, Some("config"))
            .await
            .unwrap();

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
