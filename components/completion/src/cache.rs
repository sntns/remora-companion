use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

use crate::{Candidate, Kind};

/// Values a provider fetched from far away (the platform), kept between
/// Tabs: a shell waits on the provider at every one, and a network call
/// each time would be felt. Best effort throughout -- a cache that can't
/// be read or written is just a cache miss.
///
/// One file per scope and kind, `value\thelp` per line, under the user's
/// cache directory: `$XDG_CACHE_HOME` (or `~/.cache`), `%LOCALAPPDATA%` on
/// Windows, then `<app>/completion/`.
pub struct Cache {
    path: PathBuf,
}

impl Cache {
    /// `app`'s cache of `kind` values for `scope`: whatever the values
    /// depend on, e.g. a context and the role it acts as. Any character
    /// that isn't safe in a file name is replaced.
    pub fn new(app: &str, scope: &str, kind: Kind) -> Self {
        let base = if cfg!(windows) {
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("XDG_CACHE_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::home_dir().map(|home| home.join(".cache")))
        }
        .unwrap_or_else(std::env::temp_dir);
        let scope: String = scope
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "._+-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Self {
            path: base
                .join(app)
                .join("completion")
                .join(format!("{scope}.{kind:?}").to_lowercase()),
        }
    }

    /// The values stored at most `max_age` ago.
    pub fn fresh(&self, max_age: Duration) -> Option<Vec<Candidate>> {
        let modified = std::fs::metadata(&self.path).ok()?.modified().ok()?;
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::MAX);
        if age > max_age {
            return None;
        }
        self.last()
    }

    /// The values last stored, however old: better than nothing when
    /// fetching them anew fails.
    pub fn last(&self) -> Option<Vec<Candidate>> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        Some(
            text.lines()
                .filter(|line| !line.is_empty())
                .map(|line| match line.split_once('\t') {
                    Some((value, help)) if !help.is_empty() => Candidate::new(value).help(help),
                    Some((value, _)) => Candidate::new(value),
                    None => Candidate::new(line),
                })
                .collect(),
        )
    }

    /// Stores `values`, replacing what was there.
    pub fn store(&self, values: &[Candidate]) {
        let text: String = values
            .iter()
            .map(|c| format!("{}\t{}\n", c.value, c.help.as_deref().unwrap_or_default()))
            .collect();
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_and_age() {
        let root = std::env::temp_dir().join(format!("remora-completion-{}", std::process::id()));
        let cache = Cache {
            path: root.join("eu2.as-urn_x.device"),
        };
        assert!(cache.last().is_none());
        let values = [
            Candidate::new("525400C0FFEE").help("online"),
            Candidate::new("B827EB9D6166"),
        ];
        cache.store(&values);
        assert_eq!(cache.fresh(Duration::from_secs(60)).unwrap(), values);
        assert_eq!(cache.last().unwrap(), values);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn scopes_become_safe_file_names() {
        let cache = Cache::new("rmra", "eu2.as-urn:sntns:iam:x:role:ops/..", Kind::Device);
        let name = cache
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(name, "eu2.as-urn_sntns_iam_x_role_ops_...device");
        assert!(cache.path.parent().unwrap().ends_with("rmra/completion"));
    }
}
