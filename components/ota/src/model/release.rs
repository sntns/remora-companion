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

/// Whether the platform takes `name` as an artifact's file name: one path
/// segment of `[A-Za-z0-9._+~@-]`, not starting with a dot.
pub fn is_artifact_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '~' | '@' | '-'))
}

/// An upload's resume token as the platform takes it, a UUID (hyphenated,
/// lowercase); `None` for anything else. Its 32 bare hex digits are taken
/// too, hyphenated.
pub fn upload_token(token: &str) -> Option<String> {
    let hex: String = token.chars().filter(|c| *c != '-').collect();
    let hyphens_in_place = !token.contains('-')
        || (token.len() == 36 && [8, 13, 18, 23].iter().all(|&i| &token[i..=i] == "-"));
    if hex.len() != 32 || !hyphens_in_place || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let hex = hex.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
mod upload_tests {
    use super::*;

    #[test]
    fn artifact_file_names() {
        for good in ["update-rp5.raucb", "disk.wic.bmaptar", "a+b~c@d_e.1"] {
            assert!(is_artifact_file_name(good), "{good}");
        }
        for bad in ["", ".hidden", "dir/file", "with space", "é.raucb", "a:b"] {
            assert!(!is_artifact_file_name(bad), "{bad}");
        }
    }

    #[test]
    fn upload_tokens() {
        let uuid = "3c9609be-f00b-4fe9-9319-06d8be517230";
        assert_eq!(upload_token(uuid).as_deref(), Some(uuid));
        assert_eq!(
            upload_token("3C9609BEF00B4FE9931906D8BE517230").as_deref(),
            Some(uuid)
        );
        for bad in [
            "token-1",
            "",
            "3c9609bef00b4fe9931906d8be51723",
            "3c9609be-f00b4fe9-9319-06d8be517230x",
        ] {
            assert_eq!(upload_token(bad), None, "{bad}");
        }
    }
}
