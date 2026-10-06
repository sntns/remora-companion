use std::{
    fs::{self, File},
    io::{Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use error_stack::{Report, ResultExt};
use remora_fs_walk::{FsWalkAdapterService, WalkEntryKind};
use remora_image::{
    adapter::{
        ext4::Ext4AdapterService, partition_fs::PartitionFilesystem,
        partition_table::PartitionTableAdapterService,
    },
    application::{Error, ImageServiceInterface, Result},
    model::{
        CpDirRequest, FsKind, InjectRequest, MkdirRequest, PartitionEntry, PartitionRole,
        PartitionTable,
    },
};
use remora_progress::OperationContext;

/// The image vertical's use case: partition-table reading/selection, and
/// dispatching writes to whichever of the ext4/vfat backends the target
/// partition's own on-disk signature says it is — never assumed from a
/// boot-mode label. `ext4` serves the standalone `ext4_file_*` operations,
/// whose image is a filesystem in its own right rather than a partition.
///
/// Every operation is blocking file I/O on an image that can be gigabytes,
/// so each runs on the blocking pool (see `blocking`), on a clone of this
/// controller (every field is a cheap shared handle).
#[derive(Clone)]
pub struct ImageControllerImpl {
    partition_table: PartitionTableAdapterService,
    ext4_fs: Arc<dyn PartitionFilesystem>,
    vfat_fs: Arc<dyn PartitionFilesystem>,
    fs_walk: FsWalkAdapterService,
    ext4: Ext4AdapterService,
}

impl ImageControllerImpl {
    pub fn new(
        partition_table: PartitionTableAdapterService,
        ext4_fs: Arc<dyn PartitionFilesystem>,
        vfat_fs: Arc<dyn PartitionFilesystem>,
        fs_walk: FsWalkAdapterService,
        ext4: Ext4AdapterService,
    ) -> Self {
        Self {
            partition_table,
            ext4_fs,
            vfat_fs,
            fs_walk,
            ext4,
        }
    }

    /// Run `op` on the blocking pool, off the runtime threads.
    async fn blocking<T: Send + 'static>(
        &self,
        op: impl FnOnce(&Self) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || op(&this))
            .await
            .expect("image worker panicked")
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

    fn cp_dir_blocking(&self, request: &CpDirRequest, ctx: &OperationContext) -> Result<()> {
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

        let total = walked.len() as u64;
        for (i, walked_entry) in walked.into_iter().enumerate() {
            if ctx.cancel.is_cancelled() {
                return Err(Report::new(Error::Cancelled));
            }
            ctx.sink.progress(i as u64, total);
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
                        .change_context_lazy(|| Error::ReadSource(source.clone()))?;
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
        ctx.sink.progress(total, total);

        Ok(())
    }
}

#[async_trait::async_trait]
impl ImageServiceInterface for ImageControllerImpl {
    async fn inspect(&self, path: &Path) -> Result<PartitionTable> {
        let path = path.to_path_buf();
        self.blocking(move |this| {
            this.partition_table
                .read(&path)
                .change_context(Error::ReadPartitionTable)
        })
        .await
    }

    async fn inject(&self, request: &InjectRequest) -> Result<()> {
        let request = request.clone();
        self.blocking(move |this| {
            let (entry, backend) = this.resolve(&request.image, |table| {
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
        })
        .await
    }

    async fn mkdir(&self, request: &MkdirRequest) -> Result<()> {
        let request = request.clone();
        self.blocking(move |this| {
            let (entry, backend) = this.resolve(&request.image, |table| {
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
        })
        .await
    }

    async fn cp_dir(&self, request: &CpDirRequest, ctx: &OperationContext) -> Result<()> {
        let request = request.clone();
        let ctx = ctx.clone();
        self.blocking(move |this| this.cp_dir_blocking(&request, &ctx))
            .await
    }

    async fn inject_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<()> {
        let (image, dest_path, contents) = (
            image.to_path_buf(),
            dest_path.to_string(),
            contents.to_vec(),
        );
        self.blocking(move |this| {
            let (entry, backend) = this.resolve_by_role(&image, role)?;
            backend
                .write_file(
                    &image,
                    entry.start_bytes,
                    entry.size_bytes,
                    &dest_path,
                    &contents,
                    mode,
                )
                .change_context(Error::Write)
        })
        .await
    }

    async fn ensure_dir_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
        mode: u16,
    ) -> Result<()> {
        let (image, dest_path) = (image.to_path_buf(), dest_path.to_string());
        self.blocking(move |this| {
            let (entry, backend) = this.resolve_by_role(&image, role)?;
            backend
                .ensure_dir(
                    &image,
                    entry.start_bytes,
                    entry.size_bytes,
                    &dest_path,
                    mode,
                )
                .change_context(Error::Write)
        })
        .await
    }

    async fn exists_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
    ) -> Result<bool> {
        let (image, dest_path) = (image.to_path_buf(), dest_path.to_string());
        self.blocking(move |this| {
            let (entry, backend) = this.resolve_by_role(&image, role)?;
            backend
                .exists(&image, entry.start_bytes, entry.size_bytes, &dest_path)
                .change_context(Error::Read)
        })
        .await
    }

    async fn read_file_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
    ) -> Result<Vec<u8>> {
        let (image, dest_path) = (image.to_path_buf(), dest_path.to_string());
        self.blocking(move |this| {
            let (entry, backend) = this.resolve_by_role(&image, role)?;
            backend
                .read_file(&image, entry.start_bytes, entry.size_bytes, &dest_path)
                .change_context(Error::Read)
        })
        .await
    }

    async fn ext4_file_format(
        &self,
        image: &Path,
        size_bytes: u64,
        block_size: u32,
        label: Option<&str>,
    ) -> Result<()> {
        let (image, label) = (image.to_path_buf(), label.map(str::to_string));
        self.blocking(move |this| {
            this.ext4
                .format(&image, size_bytes, block_size, label.as_deref())
                .change_context_lazy(|| Error::Format(image.clone()))
        })
        .await
    }

    async fn ext4_file_write(
        &self,
        image: &Path,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<()> {
        let (image, dest_path, contents) = (
            image.to_path_buf(),
            dest_path.to_string(),
            contents.to_vec(),
        );
        self.blocking(move |this| {
            let size = file_len(&image)?;
            this.ext4
                .write_file(&image, 0, size, &dest_path, &contents, mode)
                .change_context(Error::Write)
        })
        .await
    }

    async fn ext4_file_mkdir(&self, image: &Path, dest_path: &str, mode: u16) -> Result<()> {
        let (image, dest_path) = (image.to_path_buf(), dest_path.to_string());
        self.blocking(move |this| {
            let size = file_len(&image)?;
            this.ext4
                .create_dir(&image, 0, size, &dest_path, mode)
                .change_context(Error::Write)
        })
        .await
    }

    async fn ext4_file_exists(&self, image: &Path, dest_path: &str) -> Result<bool> {
        let (image, dest_path) = (image.to_path_buf(), dest_path.to_string());
        self.blocking(move |this| {
            let size = file_len(&image)?;
            this.ext4
                .exists(&image, 0, size, &dest_path)
                .change_context(Error::Read)
        })
        .await
    }
}

/// A standalone filesystem image's whole length -- by seeking, since a
/// block device's metadata length is 0.
fn file_len(image: &Path) -> Result<u64> {
    File::open(image)
        .and_then(|mut file| file.seek(SeekFrom::End(0)))
        .change_context_lazy(|| Error::ReadSource(image.to_path_buf()))
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
