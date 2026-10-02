#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Context(String),
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
    #[error("the upload was cancelled")]
    Cancelled,
    #[error("failed to find the devices to deploy to")]
    Targets,
    #[error("no device matches {0}")]
    NoTargets(String),
    #[error("--name only applies to a deployment to a single device")]
    NameForMany,
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

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
