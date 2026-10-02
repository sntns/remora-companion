use std::path::PathBuf;

use super::Labels;

/// A version devices can be updated to, and the artifacts that make it up.
#[derive(Debug, Clone)]
pub struct Release {
    pub name: String,
    pub version: String,
    pub labels: Labels,
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone)]
pub struct ReleaseSummary {
    pub name: String,
    pub version: String,
}

/// One file of a release, e.g. a RAUC bundle for one board.
#[derive(Debug, Clone)]
pub struct Artifact {
    /// The name update clients see.
    pub file_name: String,
    pub content_type: String,
    pub content_length: u64,
    pub checksum_sha256: String,
    /// Which devices this artifact is for, as a boolean expression over
    /// device tags (`type:rauc && (board:hdc || board:rp5)`); empty is all.
    pub tag_condition: String,
}

/// Uploading one local file as an artifact of a release.
#[derive(Debug, Clone)]
pub struct UploadRequest {
    pub release: String,
    pub path: PathBuf,
    /// The name update clients see; defaults to the file's own name.
    pub file_name: Option<String>,
    /// Defaults from the extension (`.raucb` is a RAUC bundle).
    pub content_type: Option<String>,
    pub tag_condition: String,
    /// The resume token of an interrupted upload of this same file.
    pub resume: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UploadOutcome {
    pub file_name: String,
    pub content_type: String,
    pub bytes: u64,
    /// Where this run started from: 0 for a fresh upload, the platform's
    /// offset when continuing an earlier one with a resume token.
    pub resumed_from: u64,
    /// How many times this run resumed after a dropped connection.
    pub resumes: usize,
    /// The token that resumes this upload, should it be interrupted again.
    pub content_id: String,
}
