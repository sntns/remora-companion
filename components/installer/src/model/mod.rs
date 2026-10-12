use std::path::PathBuf;

/// Replacing an installer's payload: see
/// `InstallerServiceInterface::pack`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PackRequest {
    /// The installer to start from (e.g.
    /// `remora-installer-f3apl.wic.bmaptar`).
    pub installer: PathBuf,
    /// The disk image it will flash: a `.bmaptar`, or any other format
    /// `convert` reads (a raw `.wic` just provisioned, say).
    pub image: PathBuf,
    /// The new installer, its format named by its extension.
    pub output: PathBuf,
}

/// What a pack made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackOutcome {
    /// The embedded `.bmaptar`'s size.
    pub payload_bytes: u64,
    /// The payload partition's index, and its new size.
    pub partition_index: u32,
    pub partition_bytes: u64,
    /// Whether the image had to be bundled as a `.bmaptar` first.
    pub bundled: bool,
}

/// Taking an installer's payload back out: see
/// `InstallerServiceInterface::unpack`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UnpackRequest {
    /// The installer (e.g. `laplaylist-installer-f3apl.wic.bmaptar`), in
    /// any format `convert` reads.
    pub installer: PathBuf,
    /// The disk image it flashes, written as the `.bmaptar` it holds it
    /// as, e.g. `laplaylist-image-f3apl.wic.bmaptar`.
    pub output: PathBuf,
}

/// What an unpack took out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnpackOutcome {
    /// The `.bmaptar` written.
    pub payload_bytes: u64,
    /// The payload partition it was in.
    pub partition_index: u32,
}
