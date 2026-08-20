use std::{fs, path::PathBuf};

use crate::{
    adapter::{ext4, partition_table},
    application::{ext4image, image::fs_dispatch},
    model::partition_table::PartitionRole,
};

use super::error::Error;

/// Default size/block-size for a freshly-built `config.ext4`, matching
/// `remora-config.bbclass`'s own `dd if=/dev/zero ... bs=8M count=1`.
const DEFAULT_CONFIG_SIZE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_CONFIG_BLOCK_SIZE: u32 = 1024;

/// Add or update one file inside `Shared:/remora/<slot>/config` — the
/// "simple routine to upload into the config" the Phase 5 plan calls for.
///
/// `config.ext4` itself is a regular file living on the *shared* partition
/// (vfat on EFI/UBOOT/RPI images, ext4 on BIOS — resolved automatically, no
/// `--boot-mode` needed, see `model::partition_table::FsKind`) whose own
/// *content* is a complete ext4 filesystem. If it doesn't exist yet, a fresh
/// one is built first (seeded with the default `tzdata/` dir, matching
/// `default-config.bb`); either way, its bytes are pulled out to a local
/// temp file, updated with `dest_relative_path`, and written back.
pub fn upload(
    image: &std::path::Path,
    source: &std::path::Path,
    dest_relative_path: &str,
    slot: &str,
    mode: u16,
) -> Result<(), Error> {
    let table = partition_table::read(image)?;
    let entry = table.select_role(PartitionRole::Shared)?;
    let config_path = format!("/remora/{slot}/config");

    let contents = fs::read(source).map_err(|source_err| Error::ReadSource {
        path: source.to_path_buf(),
        source: source_err,
    })?;

    // `/remora`/`/remora/<slot>` are created at Yocto build time by
    // remora-identity.bbclass/remora-config.bbclass on a real image, but
    // ensure they exist here too rather than assuming it.
    fs_dispatch::ensure_dir(image, entry.start_bytes, entry.size_bytes, "/remora", 0o755)?;
    fs_dispatch::ensure_dir(
        image,
        entry.start_bytes,
        entry.size_bytes,
        &format!("/remora/{slot}"),
        0o755,
    )?;

    let local_image = temp_path("config-local");
    let config_exists =
        fs_dispatch::exists(image, entry.start_bytes, entry.size_bytes, &config_path)?;

    if config_exists {
        let existing =
            fs_dispatch::read_file(image, entry.start_bytes, entry.size_bytes, &config_path)?;
        fs::write(&local_image, &existing).map_err(|source| Error::WriteTempImage {
            path: local_image.clone(),
            source,
        })?;
    } else {
        let seed_dir = temp_path("config-seed");
        fs::create_dir_all(seed_dir.join("tzdata")).map_err(|source| Error::WriteTempImage {
            path: seed_dir.clone(),
            source,
        })?;
        ext4image::build(
            &seed_dir,
            &local_image,
            DEFAULT_CONFIG_SIZE_BYTES,
            DEFAULT_CONFIG_BLOCK_SIZE,
            Some("config"),
        )?;
        let _ = fs::remove_dir_all(&seed_dir);
    }

    let local_size = fs::metadata(&local_image)
        .map_err(|source| Error::ReadTempImage {
            path: local_image.clone(),
            source,
        })?
        .len();
    ext4::write_file(
        &local_image,
        0,
        local_size,
        dest_relative_path,
        &contents,
        mode,
    )?;

    let updated = fs::read(&local_image).map_err(|source| Error::ReadTempImage {
        path: local_image.clone(),
        source,
    })?;
    fs_dispatch::write_file(
        image,
        entry.start_bytes,
        entry.size_bytes,
        &config_path,
        &updated,
        0o644,
    )?;

    let _ = fs::remove_file(&local_image);
    Ok(())
}

fn temp_path(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "remora-etcher-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path
}
