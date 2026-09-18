use std::path::Path;

use remora_etcher_progress::OperationContext;

use crate::model::{CpDirRequest, InjectRequest, MkdirRequest, PartitionRole, PartitionTable};

use super::error::Result;

/// The image vertical's application-facing port: what every transport (CLI
/// today, anything else later) calls into — and what other verticals
/// (identity, config) call into to write generated files into a partition
/// without needing to know its byte offset/size, or (via the `_by_role`
/// methods) the image's boot mode.
#[async_trait::async_trait]
pub trait ImageServiceInterface: Send + Sync {
    /// Read the partition table of an image file or block device. Table
    /// kind (MBR vs. GPT) is auto-detected.
    async fn inspect(&self, path: &Path) -> Result<PartitionTable>;

    /// Inject `request.source`'s content into `request.dest_path` inside
    /// whichever partition `request.partition` resolves to. The parent
    /// directory of `dest_path` must already exist — this does not create
    /// intermediate directories (narrow surface, matches `mkdir`).
    async fn inject(&self, request: &InjectRequest) -> Result<()>;

    /// Create `request.dest_path` inside whichever partition
    /// `request.partition` resolves to.
    async fn mkdir(&self, request: &MkdirRequest) -> Result<()>;

    /// Recursively copy `request.source_dir`'s content into
    /// `request.dest_path` inside whichever partition `request.partition`
    /// resolves to, preserving each entry's relative path and host file
    /// mode. `request.dest_path` itself must already exist — this does not
    /// create it (same narrow-surface rule as `inject`/`mkdir`; use `mkdir`
    /// first if needed). Symlinks inside `source_dir` are rejected — neither
    /// backend can represent them.
    ///
    /// Reports one `Progress` event per file copied through `ctx` — the
    /// only image operation that walks a caller-controlled number of files,
    /// so the only one worth it.
    async fn cp_dir(&self, request: &CpDirRequest, ctx: &OperationContext) -> Result<()>;

    /// Write `contents` to `dest_path` inside whichever partition has role
    /// `role` — resolved from the table's own detected kind (GPT/MBR), no
    /// boot mode needed (unlike `inject`, which resolves a caller-supplied
    /// `PartitionSelector::Role` and *does* require one). Used by verticals
    /// that generate their own content (identity, config) rather than
    /// acting on a CLI-supplied `--partition`/`--boot-mode` pair.
    async fn inject_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<()>;

    /// Like `inject_by_role`, but creates `dest_path` as a directory,
    /// treating "already exists" as success (see
    /// `adapter::partition_fs::PartitionFilesystem::ensure_dir`).
    async fn ensure_dir_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
        mode: u16,
    ) -> Result<()>;

    /// Whether `dest_path` exists inside whichever partition has role
    /// `role`. See `inject_by_role` for why this resolves by role alone.
    async fn exists_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
    ) -> Result<bool>;

    /// Read the whole content of `dest_path` inside whichever partition has
    /// role `role`. See `inject_by_role` for why this resolves by role
    /// alone.
    async fn read_file_by_role(
        &self,
        image: &Path,
        role: PartitionRole,
        dest_path: &str,
    ) -> Result<Vec<u8>>;
}

/// Injectable handle to whatever `ImageServiceInterface` implementation was
/// wired at startup (normally `remora-etcher-image-application`'s
/// `ImageControllerImpl`).
#[derive(Clone)]
pub struct ImageService(busybody::Service<Box<dyn ImageServiceInterface>>);

impl ImageService {
    pub fn new<T: ImageServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for ImageService {
    type Target = busybody::Service<Box<dyn ImageServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
