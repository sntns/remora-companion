use std::{fs, path::Path};

use error_stack::{Report, ResultExt};
use remora_etcher_config::application::{ConfigServiceInterface, Error, Result};
use remora_etcher_fs_walk::{FsWalkAdapterService, WalkEntryKind};
use remora_etcher_image::{
    adapter::ext4::Ext4AdapterService, application::ImageService, model::PartitionRole,
};
use remora_etcher_squashfs::application::BuildSummary;

/// Default size/block-size for a freshly-built `config.ext4`, matching
/// `remora-config.bbclass`'s own `dd if=/dev/zero ... bs=8M count=1`.
const DEFAULT_CONFIG_SIZE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_CONFIG_BLOCK_SIZE: u32 = 1024;

/// The config vertical's use case: `build` walks a host directory (via the
/// injected `FsWalkAdapterService`) and populates a fresh ext4 image with it
/// (via the injected `Ext4AdapterService`, addressed at `offset = 0` since
/// the image *is* the whole filesystem, not a partition inside one).
/// `upload` composes `build` with the injected `ImageService` (the config
/// vertical's cross-vertical dependency) to read/write `config.ext4` as a
/// regular file living on the image's shared partition.
pub struct ConfigControllerImpl {
    ext4: Ext4AdapterService,
    fs_walk: FsWalkAdapterService,
    image: ImageService,
}

impl ConfigControllerImpl {
    pub fn new(
        ext4: Ext4AdapterService,
        fs_walk: FsWalkAdapterService,
        image: ImageService,
    ) -> Self {
        Self {
            ext4,
            fs_walk,
            image,
        }
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
        self.ext4
            .format(output, size_bytes, block_size, label)
            .change_context(Error::Format(output.to_path_buf()))?;

        let walked = self
            .fs_walk
            .walk_dir(source_dir)
            .change_context(Error::Walk(source_dir.to_path_buf()))?;

        let mut entry_count = 0usize;
        let mut bytes_written = 0u64;

        for entry in walked {
            let dest_path = to_ext4_path(&entry.path);
            let mode = entry.metadata.permissions;

            match &entry.kind {
                WalkEntryKind::Directory => {
                    self.ext4
                        .create_dir(output, 0, size_bytes, &dest_path, mode)
                        .change_context_lazy(|| Error::Populate(output.to_path_buf()))?;
                }
                WalkEntryKind::File { source } => {
                    let contents = fs::read(source)
                        .map_err(|_| Report::new(Error::ReadFile(source.clone())))?;
                    bytes_written += contents.len() as u64;
                    self.ext4
                        .write_file(output, 0, size_bytes, &dest_path, &contents, mode)
                        .change_context_lazy(|| Error::Populate(output.to_path_buf()))?;
                }
                WalkEntryKind::Symlink { .. } => {
                    return Err(Report::new(Error::UnsupportedEntry(entry.path)));
                }
            }
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

        let contents =
            fs::read(source).map_err(|_| Report::new(Error::ReadSource(source.to_path_buf())))?;

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

        let local_image = remora_etcher_scratch::unique_path("remora-etcher-config-local");
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
            fs::write(&local_image, &existing)
                .map_err(|_| Report::new(Error::WriteTempImage(local_image.clone())))?;
        } else {
            let seed_dir = remora_etcher_scratch::unique_path("remora-etcher-config-seed");
            fs::create_dir_all(seed_dir.join("tzdata"))
                .map_err(|_| Report::new(Error::WriteTempImage(seed_dir.clone())))?;
            self.build(
                &seed_dir,
                &local_image,
                DEFAULT_CONFIG_SIZE_BYTES,
                DEFAULT_CONFIG_BLOCK_SIZE,
                Some("config"),
            )
            .await?;
            let _ = fs::remove_dir_all(&seed_dir);
        }

        let local_size = fs::metadata(&local_image)
            .map_err(|_| Report::new(Error::ReadTempImage(local_image.clone())))?
            .len();
        self.ext4
            .write_file(
                &local_image,
                0,
                local_size,
                dest_relative_path,
                &contents,
                mode,
            )
            .change_context_lazy(|| Error::Populate(local_image.clone()))?;

        let updated = fs::read(&local_image)
            .map_err(|_| Report::new(Error::ReadTempImage(local_image.clone())))?;
        self.image
            .inject_by_role(image, PartitionRole::Shared, &config_path, &updated, 0o644)
            .await
            .change_context(Error::Image)?;

        let _ = fs::remove_file(&local_image);
        Ok(())
    }
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
        let image = ImageService::new(remora_etcher_image_application::ImageControllerImpl::new(
            remora_etcher_image::adapter::partition_table::PartitionTableAdapterService::new(
                remora_etcher_image_adapter_partition_table::PartitionTableAdapterImpl,
            ),
            Arc::new(remora_etcher_image_adapter_ext4::Ext4AdapterImpl),
            Arc::new(remora_etcher_image_adapter_vfat::VfatAdapterImpl),
            FsWalkAdapterService::new(remora_etcher_fs_walk::FsWalkAdapterImpl),
        ));
        ConfigControllerImpl::new(
            Ext4AdapterService::new(remora_etcher_image_adapter_ext4::Ext4AdapterImpl),
            FsWalkAdapterService::new(remora_etcher_fs_walk::FsWalkAdapterImpl),
            image,
        )
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "remora-etcher-config-application-test-{}-{}",
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
