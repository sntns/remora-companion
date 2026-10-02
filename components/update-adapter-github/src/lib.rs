mod feed;
mod installer;

pub use feed::GithubReleaseFeedImpl;
pub use installer::DistInstallerImpl;

/// Where rmra and remora-etcher are published: this repository's own
/// GitHub releases (see dist-workspace.toml).
pub const RELEASES_REPO: &str = "sntns/remora-companion";
