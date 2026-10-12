use std::path::PathBuf;

use super::partition_table::PartitionSelector;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InjectRequest {
    pub image: PathBuf,
    pub source: PathBuf,
    /// Absolute path inside the partition's filesystem, e.g.
    /// `/play/tplst-app-config/config.json`. Its parent directory must
    /// already exist.
    pub dest_path: String,
    pub partition: PartitionSelector,
    /// File mode (permission bits only), e.g. `0o644`.
    pub mode: u16,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MkdirRequest {
    pub image: PathBuf,
    /// Absolute path inside the partition's filesystem, e.g.
    /// `/play/tplst-app-config`. Its parent directory must already exist —
    /// not recursive, same narrow-surface rule as `InjectRequest`.
    pub dest_path: String,
    pub partition: PartitionSelector,
    /// Directory mode (permission bits only), e.g. `0o755`.
    pub mode: u16,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CpDirRequest {
    pub image: PathBuf,
    /// Host directory to copy in, recursively.
    pub source_dir: PathBuf,
    /// Directory inside the partition's filesystem that `source_dir`'s
    /// content is copied under, e.g. `/` or `/dump`. Must already exist —
    /// not created by this request (see `mkdir`).
    pub dest_path: String,
    pub partition: PartitionSelector,
    /// Owner of every entry copied in, e.g. the device's app user for its
    /// data. None leaves them to root, as the image's builder would. Only
    /// ext4 has owners: a vfat partition refuses one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
}

/// Numeric owner of a file inside a partition: the device's own ids, which
/// the host's user database knows nothing about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

impl std::str::FromStr for Owner {
    type Err = String;

    /// `UID:GID`, e.g. `1000:1000`.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let (uid, gid) = s
            .split_once(':')
            .ok_or_else(|| format!("expected UID:GID, got {s:?}"))?;
        let id = |part: &str| {
            part.parse::<u32>()
                .map_err(|_| format!("expected numeric UID:GID, got {s:?}"))
        };
        Ok(Self {
            uid: id(uid)?,
            gid: id(gid)?,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FillRequest {
    pub image: PathBuf,
    /// Host file whose bytes become the partition's content, as is: no
    /// filesystem (e.g. an installer's `.bmaptar` payload).
    pub source: PathBuf,
    /// Must resolve to the image's last partition (GPT, or a primary one on
    /// MBR), which is resized to fit `source`.
    pub partition: PartitionSelector,
}

/// What filling a partition made of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillOutcome {
    pub index: u32,
    /// The source's length.
    pub content_bytes: u64,
    /// The partition's new size: the content rounded up to
    /// [`FILL_ALIGNMENT`].
    pub partition_bytes: u64,
}

/// A filled partition's size is a multiple of this: wic's own 1 MiB
/// alignment, so a resized partition ends where a freshly built one would.
pub const FILL_ALIGNMENT: u64 = 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_parses_numeric_uid_gid() {
        assert_eq!(
            "1000:1001".parse::<Owner>(),
            Ok(Owner {
                uid: 1000,
                gid: 1001
            })
        );
        for bad in ["1000", "laplaylist:laplaylist", "1000:", ":1000", "-1:0"] {
            assert!(bad.parse::<Owner>().is_err(), "{bad}");
        }
    }

    #[test]
    fn cp_dir_request_owner_is_optional_in_json() {
        let request: CpDirRequest = serde_json::from_str(
            r#"{"image":"i","source_dir":"s","dest_path":"/","partition":{"role":"data"}}"#,
        )
        .unwrap();
        assert_eq!(request.owner, None);
        let request: CpDirRequest = serde_json::from_str(
            r#"{"image":"i","source_dir":"s","dest_path":"/","partition":{"role":"data"},
                "owner":{"uid":1000,"gid":1000}}"#,
        )
        .unwrap();
        assert_eq!(
            request.owner,
            Some(Owner {
                uid: 1000,
                gid: 1000
            })
        );
    }
}
