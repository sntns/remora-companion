use std::{fs, path::Path, sync::Arc};

use error_stack::{Report, ResultExt};
use remora_etcher_fs_walk::{FsWalkAdapterService, WalkEntryKind};
use remora_etcher_image::{
    adapter::{partition_fs::PartitionFilesystem, partition_table::PartitionTableAdapterService},
    application::{Error, ImageServiceInterface, Result},
    model::{
        CpDirRequest, FsKind, InjectRequest, MkdirRequest, PartitionEntry, PartitionRole,
        PartitionTable,
    },
};

/// The image vertical's use case: partition-table reading/selection, and
/// dispatching writes to whichever of the ext4/vfat backends the target
/// partition's own on-disk signature says it is — never assumed from a
/// boot-mode label.
pub struct ImageControllerImpl {
    partition_table: PartitionTableAdapterService,
    ext4_fs: Arc<dyn PartitionFilesystem>,
    vfat_fs: Arc<dyn PartitionFilesystem>,
    fs_walk: FsWalkAdapterService,
}

impl ImageControllerImpl {
    pub fn new(
        partition_table: PartitionTableAdapterService,
        ext4_fs: Arc<dyn PartitionFilesystem>,
        vfat_fs: Arc<dyn PartitionFilesystem>,
        fs_walk: FsWalkAdapterService,
    ) -> Self {
        Self {
            partition_table,
            ext4_fs,
            vfat_fs,
            fs_walk,
        }
    }

    fn backend(&self, kind: FsKind) -> &dyn PartitionFilesystem {
        match kind {
            FsKind::Ext4 => self.ext4_fs.as_ref(),
            FsKind::Vfat => self.vfat_fs.as_ref(),
        }
    }

    /// Read `image`'s partition table, resolve `entry` via `select`, and
    /// detect which backend occupies it.
    fn resolve(
        &self,
        image: &Path,
        select: impl FnOnce(&PartitionTable) -> Result<PartitionEntry>,
    ) -> Result<(PartitionEntry, &dyn PartitionFilesystem)> {
        let table = self
            .partition_table
            .read(image)
            .change_context(Error::ReadPartitionTable)?;
        let entry = select(&table)?;
        let kind = self
            .partition_table
            .detect_fs_kind(image, entry.start_bytes)
            .change_context(Error::ReadPartitionTable)?;
        Ok((entry, self.backend(kind)))
    }

    fn resolve_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
    ) -> Result<(PartitionEntry, &dyn PartitionFilesystem)> {
        self.resolve(image, |table| {
            table
                .select_role(role)
                .cloned()
                .change_context(Error::SelectPartition)
        })
    }
}

impl ImageServiceInterface for ImageControllerImpl {
    fn inspect(&self, path: &Path) -> Result<PartitionTable> {
        self.partition_table
            .read(path)
            .change_context(Error::ReadPartitionTable)
    }

    fn inject(&self, request: &InjectRequest) -> Result<()> {
        let (entry, backend) = self.resolve(&request.image, |table| {
            table
                .select(request.partition, request.boot_mode)
                .cloned()
                .change_context(Error::SelectPartition)
        })?;

        let contents = fs::read(&request.source)
            .change_context_lazy(|| Error::ReadSource(request.source.clone()))?;

        backend
            .write_file(
                &request.image,
                entry.start_bytes,
                entry.size_bytes,
                &request.dest_path,
                &contents,
                request.mode,
            )
            .change_context(Error::Write)
    }

    fn mkdir(&self, request: &MkdirRequest) -> Result<()> {
        let (entry, backend) = self.resolve(&request.image, |table| {
            table
                .select(request.partition, request.boot_mode)
                .cloned()
                .change_context(Error::SelectPartition)
        })?;

        backend
            .create_dir(
                &request.image,
                entry.start_bytes,
                entry.size_bytes,
                &request.dest_path,
                request.mode,
            )
            .change_context(Error::Write)
    }

    fn cp_dir(&self, request: &CpDirRequest) -> Result<()> {
        let (entry, backend) = self.resolve(&request.image, |table| {
            table
                .select(request.partition, request.boot_mode)
                .cloned()
                .change_context(Error::SelectPartition)
        })?;

        let walked = self
            .fs_walk
            .walk_dir(&request.source_dir)
            .change_context_lazy(|| Error::Walk(request.source_dir.clone()))?;

        for walked_entry in walked {
            let dest_path = to_partition_path(&request.dest_path, &walked_entry.path);
            let mode = walked_entry.metadata.permissions;

            match &walked_entry.kind {
                WalkEntryKind::Directory => {
                    backend
                        .ensure_dir(
                            &request.image,
                            entry.start_bytes,
                            entry.size_bytes,
                            &dest_path,
                            mode,
                        )
                        .change_context(Error::Write)?;
                }
                WalkEntryKind::File { source } => {
                    let contents = fs::read(source)
                        .map_err(|_| Report::new(Error::ReadSource(source.clone())))?;
                    backend
                        .write_file(
                            &request.image,
                            entry.start_bytes,
                            entry.size_bytes,
                            &dest_path,
                            &contents,
                            mode,
                        )
                        .change_context(Error::Write)?;
                }
                WalkEntryKind::Symlink { .. } => {
                    return Err(Report::new(Error::UnsupportedEntry(walked_entry.path)));
                }
            }
        }

        Ok(())
    }

    fn inject_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<()> {
        let (entry, backend) = self.resolve_by_role(image, role)?;

        backend
            .write_file(
                image,
                entry.start_bytes,
                entry.size_bytes,
                dest_path,
                contents,
                mode,
            )
            .change_context(Error::Write)
    }

    fn ensure_dir_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
        mode: u16,
    ) -> Result<()> {
        let (entry, backend) = self.resolve_by_role(image, role)?;

        backend
            .ensure_dir(image, entry.start_bytes, entry.size_bytes, dest_path, mode)
            .change_context(Error::Write)
    }

    fn exists_by_role(&self, image: &Path, role: PartitionRole, dest_path: &str) -> Result<bool> {
        let (entry, backend) = self.resolve_by_role(image, role)?;

        backend
            .exists(image, entry.start_bytes, entry.size_bytes, dest_path)
            .change_context(Error::Write)
    }

    fn read_file_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
    ) -> Result<Vec<u8>> {
        let (entry, backend) = self.resolve_by_role(image, role)?;

        backend
            .read_file(image, entry.start_bytes, entry.size_bytes, dest_path)
            .change_context(Error::Write)
    }
}

/// `rel_path` joined with `/` under `base` regardless of host
/// path-separator conventions — partition filesystem paths are always
/// `/`-separated, even when built on Windows.
fn to_partition_path(base: &str, rel_path: &Path) -> String {
    let joined = rel_path
        .iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    format!("{}/{joined}", base.trim_end_matches('/'))
}
