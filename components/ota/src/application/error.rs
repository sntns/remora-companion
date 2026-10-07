#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no usable context")]
    Context,
    #[error("failed to list releases")]
    ListReleases,
    #[error("failed to get release {0:?}")]
    GetRelease(String),
    #[error("failed to create release {0:?}")]
    CreateRelease(String),
    #[error("failed to update release {0:?}")]
    UpdateRelease(String),
    #[error("failed to delete release {0:?}")]
    DeleteRelease(String),
    /// Local, so not worth resuming: retrying wouldn't read it any better.
    #[error("failed to read {0}")]
    ReadArtifact(std::path::PathBuf),
    #[error(
        "failed to upload {file} to release {release:?} (resume it with --resume {content_id})"
    )]
    Upload {
        file: String,
        release: String,
        content_id: String,
    },
    #[error("the upload of {file} to release {release:?} was cancelled (resume it with --resume {content_id})")]
    Cancelled {
        file: String,
        release: String,
        content_id: String,
    },
    #[error("failed to download {file} of release {release:?}")]
    Download { file: String, release: String },
    #[error("failed to find the devices to deploy to")]
    Targets,
    #[error("no device matches {0}")]
    NoTargets(String),
    #[error("--name only applies to a deployment to a single device")]
    NameForMany,
    #[error("failed to create deployment {0:?}")]
    CreateDeployment(String),
    #[error("failed to list deployments")]
    ListDeployments,
    #[error("failed to get deployment {0:?}")]
    GetDeployment(String),
    #[error("failed to start deployment {0:?}")]
    StartDeployment(String),
    #[error("failed to cancel deployment {0:?}")]
    CancelDeployment(String),
    #[error("failed to delete deployment {0:?}")]
    DeleteDeployment(String),
}

impl Error {
    /// The token that continues this upload with `--resume`, when the
    /// failure left one worth continuing.
    pub fn resume_token(&self) -> Option<&str> {
        match self {
            Self::Upload { content_id, .. } | Self::Cancelled { content_id, .. } => {
                Some(content_id)
            }
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
