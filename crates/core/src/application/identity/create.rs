use std::path::PathBuf;

use crate::{
    adapter::partition_table, application::image::fs_dispatch,
    model::partition_table::PartitionRole,
};

use super::{build::build, error::Error};

/// Build identity.squashfs from `inputs`/`hostname`/`machine_id` (see
/// `build`) and inject it into `Shared:/remora/identity` of `image` in one
/// shot — the "simple routine to create an identity" the Phase 5 plan calls
/// for. Boot mode and filesystem kind (vfat/ext4) are auto-detected — no
/// `--boot-mode` flag, see `model::partition_table::FsKind`.
pub fn create(
    inputs: &[PathBuf],
    hostname: Option<&str>,
    machine_id: Option<&str>,
    image: &std::path::Path,
) -> Result<u64, Error> {
    let temp = temp_path();
    build(inputs, hostname, machine_id, &temp)?;

    let table = partition_table::read(image)?;
    let entry = table.select_role(PartitionRole::Shared)?;

    let contents = std::fs::read(&temp).map_err(|source| Error::ReadBuiltImage {
        path: temp.clone(),
        source,
    })?;
    let _ = std::fs::remove_file(&temp);

    fs_dispatch::ensure_dir(image, entry.start_bytes, entry.size_bytes, "/remora", 0o755)?;
    fs_dispatch::write_file(
        image,
        entry.start_bytes,
        entry.size_bytes,
        "/remora/identity",
        &contents,
        0o644,
    )?;

    Ok(contents.len() as u64)
}

fn temp_path() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "remora-etcher-identity-create-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path
}
