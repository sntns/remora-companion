use std::path::Path;

use super::error::Result;

/// What `build` put into the image.
#[derive(Debug)]
pub struct BuildSummary {
    pub entry_count: usize,
    pub bytes_written: u64,
}

/// The config vertical's application-facing port: what every transport (CLI
/// today, anything else later) calls into.
#[async_trait::async_trait]
pub trait ConfigServiceInterface: Send + Sync {
    /// Build a fresh ext4 image at `output` (created/truncated to
    /// `size_bytes`) populated from `source_dir` — the "format + populate"
    /// decomposition of `mkfs.ext4 -d CONFIG_DIR`.
    async fn build(
        &self,
        source_dir: &Path,
        output: &Path,
        size_bytes: u64,
        block_size: u32,
        label: Option<&str>,
    ) -> Result<BuildSummary>;

    /// Add or update one file inside `Shared:/remora/<slot>/config` of
    /// `image` — building a fresh `config.ext4` first if it doesn't exist
    /// yet (seeded with an empty `tzdata/` dir, matching
    /// `default-config.bb`). Boot mode and filesystem kind are
    /// auto-detected, same as the identity vertical's `create`.
    async fn upload(
        &self,
        image: &Path,
        source: &Path,
        dest_relative_path: &str,
        slot: &str,
        mode: u16,
    ) -> Result<()>;
}

/// Injectable handle to whatever `ConfigServiceInterface` implementation was
/// wired at startup (normally `remora-config-application`'s
/// `ConfigControllerImpl`).
#[derive(Clone)]
pub struct ConfigService(busybody::Service<Box<dyn ConfigServiceInterface>>);

impl ConfigService {
    pub fn new<T: ConfigServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for ConfigService {
    type Target = busybody::Service<Box<dyn ConfigServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
