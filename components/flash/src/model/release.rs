use std::{fmt, path::PathBuf};

use remora_context::model::ContextOverride;

/// Where the image to flash is.
#[derive(Debug, Clone)]
pub enum ImageOrigin {
    /// A local file.
    File(PathBuf),
    /// An artifact of a release, downloaded as it's flashed.
    Artifact(ReleaseArtifact),
}

impl From<PathBuf> for ImageOrigin {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
}

impl fmt::Display for ImageOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(f, "{}", path.display()),
            Self::Artifact(artifact) => write!(f, "{}", artifact),
        }
    }
}

/// One artifact of a release, as the context `over` (or the current one)
/// sees it.
#[derive(Debug, Clone)]
pub struct ReleaseArtifact {
    pub over: Option<ContextOverride>,
    pub release: String,
    pub file_name: String,
    pub size: u64,
    /// Which devices it's for, as the release says (`board:rp5 &&
    /// type:diskimage`); empty is all.
    pub tag_condition: String,
}

impl fmt::Display for ReleaseArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} of release {:?}", self.file_name, self.release)
    }
}

/// A release's disk image: an artifact tagged `type:diskimage`, for the
/// boards its tag condition names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskImage {
    pub file_name: String,
    /// The `board:` values of its tag condition; empty when it names none.
    pub boards: Vec<String>,
    pub size: u64,
    /// Its whole tag condition, as the release says.
    pub tag_condition: String,
}
