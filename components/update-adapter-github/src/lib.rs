mod feed;
mod installer;

pub use feed::GithubReleaseFeedImpl;
pub use installer::DistInstallerImpl;

/// Where rmra and remora-etcher are published: public, while their sources
/// stay private (see dist-workspace.toml).
pub const RELEASES_REPO: &str = "sntns/remora-companion-releases";
