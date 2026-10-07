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

impl Artifact {
    /// The tags its tag condition names (`board:rp5 && (type:rauc)` names
    /// `board:rp5` and `type:rauc`), whatever the operators around them.
    pub fn tags(&self) -> Vec<&str> {
        tags(&self.tag_condition)
    }

    /// The values its tag condition names for `key` (`board` gives `rp5`).
    pub fn tag_values(&self, key: &str) -> Vec<&str> {
        self.tags()
            .into_iter()
            .filter_map(|tag| tag.strip_prefix(key)?.strip_prefix(':'))
            .collect()
    }
}

/// The tags a tag condition names, whatever the operators around them.
pub fn tags(condition: &str) -> Vec<&str> {
    condition
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, ':' | '_' | '-' | '.' | '/')))
        .filter(|tag| tag.contains(':'))
        .collect()
}

/// Downloading one artifact of a release to a local file.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub release: String,
    pub file_name: String,
    pub path: PathBuf,
    /// Replace `path` when it already exists.
    pub overwrite: bool,
}

#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    pub bytes: u64,
    /// Where this run started from: 0, or what an interrupted run left.
    pub resumed_from: u64,
    /// How many times this run picked a dropped connection up again.
    pub resumes: usize,
    /// Whether the file was checked against the release's sha256 (the
    /// release may not know it).
    pub verified: bool,
}

/// Uploading one local file as an artifact of a release.
#[derive(Debug, Clone)]
pub struct UploadRequest {
    pub release: String,
    pub path: PathBuf,
    /// The name update clients see; defaults to the file's own name.
    pub file_name: Option<String>,
    /// Defaults from the extension (`.json`, `.tar`, `.gz`/`.tgz`), else
    /// `application/octet-stream` -- a RAUC bundle's too: update clients
    /// pick bundles by file name and tag condition, not content type.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_read_through_the_operators() {
        assert_eq!(
            tags("board:f3apl && type:diskimage"),
            ["board:f3apl", "type:diskimage"]
        );
        assert_eq!(
            tags("(board:rp5 || board:hdc)&&type:diskimage"),
            ["board:rp5", "board:hdc", "type:diskimage"]
        );
        assert!(tags("").is_empty());
    }
}
