use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use error_stack::{Report, ResultExt};
use remora_context::{
    adapter::{
        credentials::{self, CredentialStoreAdapter},
        store::{self, ContextStoreAdapter, StoreLock},
    },
    model::{Context, ContextKind, Contexts, Credentials, Migration},
};
use serde::{Deserialize, Serialize};

use crate::{
    layout::{create_private_dir, read_optional, write_atomic, Layout},
    migrate,
};

/// The format `config.json` is in: 2 since contexts are logins or roles
/// derived from them, all in this one file.
pub(crate) const VERSION: u32 = 2;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    /// Absent in the previous format's file.
    #[serde(default)]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_context: Option<String>,
    #[serde(default)]
    pub contexts: BTreeMap<String, Stored>,
}

/// A context as stored: its name is its key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Stored {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub kind: ContextKind,
}

impl Config {
    pub fn from_contexts(contexts: &Contexts) -> Self {
        Self {
            version: VERSION,
            current_context: contexts.current.clone(),
            contexts: contexts
                .contexts
                .iter()
                .map(|(name, context)| {
                    (
                        name.clone(),
                        Stored {
                            description: context.description.clone(),
                            kind: context.kind.clone(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn into_contexts(self) -> Contexts {
        Contexts {
            current: self.current_context,
            contexts: self
                .contexts
                .into_iter()
                .map(|(name, stored)| {
                    let context = Context {
                        name: name.clone(),
                        description: stored.description,
                        kind: stored.kind,
                    };
                    (name, context)
                })
                .collect(),
        }
    }
}

/// Every context in `<root>/config.json`, with the current one; migrated
/// from the previous one-directory-per-context layout on first use.
pub struct FileContextStoreImpl {
    layout: Layout,
    /// Whether this process holds the lock: a migration `load` starts
    /// while it does must not wait for itself.
    held: Arc<AtomicBool>,
}

impl FileContextStoreImpl {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            layout: Layout { root: root.into() },
            held: Arc::default(),
        }
    }

    /// Adds or replaces one context: for fixtures and tools that seed a
    /// store; the use case goes through the port.
    pub fn put(&self, context: &Context) -> store::Result<()> {
        let _lock = self.lock()?;
        let mut contexts = self.load()?;
        contexts.insert(context.clone());
        self.save(&contexts)
    }

    /// The config file as it is: `None` when there is none yet.
    fn read(&self) -> store::Result<Option<Config>> {
        let path = self.layout.config();
        match read_optional(&path).change_context_lazy(|| store::Error::Read(path.clone()))? {
            None => Ok(None),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .change_context_lazy(|| store::Error::Parse(path.clone())),
        }
    }

    /// Whether what's stored is in a previous format.
    fn outdated(&self, config: Option<&Config>) -> bool {
        match config {
            Some(config) => config.version < VERSION,
            None => self.layout.legacy_contexts().exists(),
        }
    }

    fn write(&self, config: &Config) -> store::Result<()> {
        let path = self.layout.config();
        let bytes = serde_json::to_vec_pretty(config)
            .change_context_lazy(|| store::Error::Write(path.clone()))?;
        write_atomic(&path, &bytes, false).change_context_lazy(|| store::Error::Write(path))
    }

    /// Migrates under the lock -- this process's own, when it holds it.
    fn migrate_locked(&self, dry_run: bool) -> store::Result<Option<Migration>> {
        let _lock = if self.held.load(Ordering::SeqCst) {
            None
        } else {
            Some(self.lock()?)
        };
        // Read again under the lock: another process may have done it.
        if !self.outdated(self.read()?.as_ref()) {
            return Ok(None);
        }
        migrate::migrate(&self.layout, dry_run)
            .change_context_lazy(|| store::Error::Migrate(self.layout.root.clone()))
            .map(Some)
    }
}

/// The lock file, open and locked: closing it unlocks it.
struct Held {
    _file: std::fs::File,
    held: Arc<AtomicBool>,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.held.store(false, Ordering::SeqCst);
    }
}

impl ContextStoreAdapter for FileContextStoreImpl {
    fn lock(&self) -> store::Result<StoreLock> {
        let path = self.layout.lock();
        let lock = || store::Error::Lock(path.clone());
        create_private_dir(&self.layout.root).change_context_lazy(lock)?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).change_context_lazy(lock)?;
        file.lock().change_context_lazy(lock)?;
        self.held.store(true, Ordering::SeqCst);
        Ok(StoreLock::new(Held {
            _file: file,
            held: self.held.clone(),
        }))
    }

    fn load(&self) -> store::Result<Contexts> {
        let mut config = self.read()?;
        if self.outdated(config.as_ref()) {
            self.migrate_locked(false)?;
            config = self.read()?;
        }
        Ok(config.map(Config::into_contexts).unwrap_or_default())
    }

    fn save(&self, contexts: &Contexts) -> store::Result<()> {
        self.write(&Config::from_contexts(contexts))
    }

    fn migrate(&self, dry_run: bool) -> store::Result<Option<Migration>> {
        if !self.outdated(self.read()?.as_ref()) {
            return Ok(None);
        }
        self.migrate_locked(dry_run)
    }
}

/// Every login's credentials in `<root>/credentials.json`, mode 0600, in
/// the clear -- the same trust as the sntns CLI's own configuration file.
/// Changed under the context store's lock, which the use case holds.
pub struct FileCredentialStoreImpl {
    layout: Layout,
}

impl FileCredentialStoreImpl {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            layout: Layout { root: root.into() },
        }
    }

    fn read(&self, context: &str) -> credentials::Result<BTreeMap<String, Credentials>> {
        let path = self.layout.credentials();
        let read = || credentials::Error::Read(context.to_owned());
        match read_optional(&path).change_context_lazy(read)? {
            None => Ok(BTreeMap::new()),
            Some(bytes) => serde_json::from_slice(&bytes)
                .change_context_lazy(read)
                .attach_with(|| path.display().to_string()),
        }
    }

    fn write(
        &self,
        all: &BTreeMap<String, Credentials>,
        error: impl Fn() -> credentials::Error,
    ) -> credentials::Result<()> {
        let path = self.layout.credentials();
        let bytes = serde_json::to_vec_pretty(all).change_context_lazy(&error)?;
        write_atomic(&path, &bytes, true)
            .change_context_lazy(&error)
            .attach_with(|| path.display().to_string())
    }
}

impl CredentialStoreAdapter for FileCredentialStoreImpl {
    fn get(&self, context: &str) -> credentials::Result<Option<Credentials>> {
        Ok(self.read(context)?.remove(context))
    }

    fn put(&self, context: &str, credentials: &Credentials) -> credentials::Result<()> {
        let mut all = self.read(context)?;
        all.insert(context.to_owned(), credentials.clone());
        self.write(&all, || credentials::Error::Write(context.to_owned()))
    }

    fn delete(&self, context: &str) -> credentials::Result<bool> {
        let mut all = self.read(context)?;
        if all.remove(context).is_none() {
            return Ok(false);
        }
        if all.is_empty() {
            let path = self.layout.credentials();
            return match std::fs::remove_file(&path) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
                Err(e) => Err(Report::new(e)
                    .change_context(credentials::Error::Delete(context.to_owned()))
                    .attach(path.display().to_string())),
            };
        }
        self.write(&all, || credentials::Error::Delete(context.to_owned()))
            .map(|()| true)
    }
}

#[cfg(test)]
mod tests {
    use remora_context::model::{Derivation, Endpoint, Secret};

    use super::*;

    fn token(token: &str) -> Credentials {
        Credentials {
            secret: Secret::AccessKey {
                token: token.into(),
            },
        }
    }

    #[test]
    fn contexts_round_trip_in_one_file() {
        let root = tempfile::tempdir().unwrap();
        let store = FileContextStoreImpl::new(root.path());
        assert_eq!(store.load().unwrap(), Contexts::default());
        assert_eq!(store.migrate(false).unwrap(), None);

        let mut contexts = Contexts::default();
        contexts.insert(Context::login("eu2", Endpoint::new("api.example")));
        contexts.insert(Context {
            name: "acme".into(),
            description: Some("Acme's hubs".into()),
            kind: ContextKind::Derived(Derivation {
                from: "eu2".into(),
                role: "urn:sntns:iam:eu2:acme:role:ops".into(),
                account: Some("Acme".into()),
            }),
        });
        contexts.current = Some("acme".into());
        {
            let _lock = store.lock().unwrap();
            store.save(&contexts).unwrap();
        }
        assert_eq!(store.load().unwrap(), contexts);

        let written = std::fs::read_to_string(root.path().join("config.json")).unwrap();
        assert!(written.contains("\"version\": 2"), "{written}");
        // The key is the name: no copy of it to drift.
        assert!(!written.contains("\"name\""), "{written}");
    }

    #[test]
    fn the_lock_is_exclusive_across_handles() {
        let root = tempfile::tempdir().unwrap();
        let store = FileContextStoreImpl::new(root.path());
        let lock = store.lock().unwrap();
        let other = std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join(".lock"))
            .unwrap();
        assert!(other.try_lock().is_err());
        drop(lock);
        assert!(other.try_lock().is_ok());
    }

    #[test]
    fn credentials_are_private_and_removable() {
        let root = tempfile::tempdir().unwrap();
        let store = FileCredentialStoreImpl::new(root.path());
        assert!(store.get("eu2").unwrap().is_none());
        store.put("eu2", &token("s3cret")).unwrap();
        store.put("dev", &token("other")).unwrap();
        assert!(store.get("eu2").unwrap() == Some(token("s3cret")));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = root.path().join("credentials.json");
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        assert!(store.delete("eu2").unwrap());
        assert!(!store.delete("eu2").unwrap());
        assert!(store.get("dev").unwrap().is_some());
        assert!(store.delete("dev").unwrap());
        assert!(!root.path().join("credentials.json").exists());
    }
}
