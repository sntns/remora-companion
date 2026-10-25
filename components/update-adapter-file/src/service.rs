use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, SystemTime},
};

use error_stack::{Report, ResultExt};
use remora_update::{
    adapter::state::{CheckStateAdapter, Error, Result},
    model::{LastCheck, Release, Version},
};
use serde::{Deserialize, Serialize};

/// `<root>/update-check.json`, one entry per binary:
///
/// ```text
/// {"rmra": {"checkedAt": 1793018096, "version": "0.21.0", "tag": "v0.21.0"}}
/// ```
///
/// Not a secret, nor precious: a file that can't be read is as good as
/// none, and the next look writes it anew.
pub struct FileCheckStateImpl {
    path: PathBuf,
}

impl FileCheckStateImpl {
    /// In `root`, the configuration directory the contexts are in.
    pub fn new(root: PathBuf) -> Self {
        Self {
            path: root.join("update-check.json"),
        }
    }

    async fn entries(&self) -> BTreeMap<String, Entry> {
        tokio::fs::read(&self.path)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    /// Seconds since the epoch.
    checked_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
}

#[async_trait::async_trait]
impl CheckStateAdapter for FileCheckStateImpl {
    async fn load(&self, app: &str) -> Result<Option<LastCheck>> {
        Ok(self.entries().await.remove(app).map(|entry| LastCheck {
            checked_at: SystemTime::UNIX_EPOCH + Duration::from_secs(entry.checked_at),
            latest: entry
                .version
                .as_deref()
                .and_then(Version::parse)
                .zip(entry.tag)
                .map(|(version, tag)| Release { version, tag }),
        }))
    }

    async fn save(&self, app: &str, check: &LastCheck) -> Result<()> {
        let mut entries = self.entries().await;
        entries.insert(
            app.to_owned(),
            Entry {
                checked_at: check
                    .checked_at
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                version: check.latest.as_ref().map(|r| r.version.to_string()),
                tag: check.latest.as_ref().map(|r| r.tag.clone()),
            },
        );
        let bytes = serde_json::to_vec_pretty(&entries).change_context(Error::Write)?;
        if let Some(dir) = self.path.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .change_context(Error::Write)?;
        }
        // Written aside then renamed: a binary reading it meanwhile sees
        // the old file or the new one, never half of one.
        let partial = self.path.with_extension("json.partial");
        tokio::fs::write(&partial, bytes)
            .await
            .change_context(Error::Write)?;
        tokio::fs::rename(&partial, &self.path)
            .await
            .map_err(|e| Report::new(Error::Write).attach(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[tokio::test]
    async fn remembers_each_binarys_last_look() {
        let dir = tempfile::tempdir().unwrap();
        let state = FileCheckStateImpl::new(dir.path().join("rmra"));
        assert_eq!(state.load("rmra").await.unwrap(), None);

        let found = LastCheck {
            checked_at: at(1_793_018_096),
            latest: Some(Release {
                version: Version::parse("0.21.0").unwrap(),
                tag: "v0.21.0".into(),
            }),
        };
        state.save("rmra", &found).await.unwrap();
        let offline = LastCheck {
            checked_at: at(1_793_000_000),
            latest: None,
        };
        state.save("remora-etcher", &offline).await.unwrap();

        assert_eq!(state.load("rmra").await.unwrap(), Some(found));
        assert_eq!(state.load("remora-etcher").await.unwrap(), Some(offline));
    }

    #[tokio::test]
    async fn an_unreadable_file_is_as_good_as_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("update-check.json"), b"{not json").unwrap();
        let state = FileCheckStateImpl::new(dir.path().to_path_buf());
        assert_eq!(state.load("rmra").await.unwrap(), None);
        let check = LastCheck {
            checked_at: at(1),
            latest: None,
        };
        state.save("rmra", &check).await.unwrap();
        assert_eq!(state.load("rmra").await.unwrap(), Some(check));
    }
}
