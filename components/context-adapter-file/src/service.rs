use std::{fs, io, path::PathBuf};

use error_stack::{Report, ResultExt};
use remora_context::{
    adapter::{
        credentials::{self, CredentialStoreAdapter},
        store::{self, ContextStoreAdapter},
    },
    model::{Context, Credentials},
};
use serde::{Deserialize, Serialize};

use crate::layout::{read_optional, write_atomic, Layout};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_context: Option<String>,
}

/// Contexts as `<root>/contexts/<name>/meta.json`, the current one in
/// `<root>/config.json`.
pub struct FileContextStoreImpl {
    layout: Layout,
}

impl FileContextStoreImpl {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            layout: Layout { root: root.into() },
        }
    }

    fn read_config(&self) -> store::Result<Config> {
        let path = self.layout.config();
        match read_optional(&path).change_context_lazy(|| store::Error::Read(path.clone()))? {
            None => Ok(Config::default()),
            Some(bytes) => serde_json::from_slice(&bytes)
                .change_context_lazy(|| store::Error::Parse(path.clone())),
        }
    }
}

impl ContextStoreAdapter for FileContextStoreImpl {
    fn list(&self) -> store::Result<Vec<Context>> {
        let dir = self.layout.contexts();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Report::new(e).change_context(store::Error::Read(dir))),
        };
        let mut contexts = Vec::new();
        for entry in entries {
            let entry = entry.change_context_lazy(|| store::Error::Read(dir.clone()))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // A directory without meta.json (a half-removed context, a stray
            // credentials file) is not a context.
            if let Some(context) = self.get(&name)? {
                contexts.push(context);
            }
        }
        contexts.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(contexts)
    }

    fn get(&self, name: &str) -> store::Result<Option<Context>> {
        let path = self.layout.meta(name);
        let Some(bytes) =
            read_optional(&path).change_context_lazy(|| store::Error::Read(path.clone()))?
        else {
            return Ok(None);
        };
        let mut context: Context = serde_json::from_slice(&bytes)
            .change_context_lazy(|| store::Error::Parse(path.clone()))?;
        // The directory is the key; a hand-edited name inside can't fork it.
        context.name = name.to_owned();
        Ok(Some(context))
    }

    fn put(&self, context: &Context) -> store::Result<()> {
        let path = self.layout.meta(&context.name);
        let bytes = serde_json::to_vec_pretty(context)
            .change_context_lazy(|| store::Error::Write(path.clone()))?;
        write_atomic(&path, &bytes, false).change_context_lazy(|| store::Error::Write(path))
    }

    fn delete(&self, name: &str) -> store::Result<()> {
        let dir = self.layout.context_dir(name);
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Report::new(e).change_context(store::Error::Write(dir))),
        }
    }

    fn current(&self) -> store::Result<Option<String>> {
        Ok(self.read_config()?.current_context)
    }

    fn set_current(&self, name: Option<&str>) -> store::Result<()> {
        let mut config = self.read_config()?;
        config.current_context = name.map(str::to_owned);
        let path = self.layout.config();
        let bytes = serde_json::to_vec_pretty(&config)
            .change_context_lazy(|| store::Error::Write(path.clone()))?;
        write_atomic(&path, &bytes, false).change_context_lazy(|| store::Error::Write(path))
    }

    fn location(&self) -> String {
        self.layout.root.display().to_string()
    }
}

/// Credentials as `<root>/contexts/<name>/credentials.json`, mode 0600, in
/// the clear -- the same trust as the sntns CLI's own configuration file,
/// until an OS-keyring `CredentialStoreAdapter` replaces this one.
pub struct FileCredentialStoreImpl {
    layout: Layout,
}

impl FileCredentialStoreImpl {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            layout: Layout { root: root.into() },
        }
    }
}

impl CredentialStoreAdapter for FileCredentialStoreImpl {
    fn get(&self, context: &str) -> credentials::Result<Option<Credentials>> {
        let path = self.layout.credentials(context);
        let read = || credentials::Error::Read(context.to_owned());
        match read_optional(&path).change_context_lazy(read)? {
            None => Ok(None),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .change_context_lazy(read)
                .attach_with(|| path.display().to_string()),
        }
    }

    fn put(&self, context: &str, credentials: &Credentials) -> credentials::Result<()> {
        let path = self.layout.credentials(context);
        let write = || credentials::Error::Write(context.to_owned());
        let bytes = serde_json::to_vec_pretty(credentials).change_context_lazy(write)?;
        write_atomic(&path, &bytes, true)
            .change_context_lazy(write)
            .attach_with(|| path.display().to_string())
    }

    fn delete(&self, context: &str) -> credentials::Result<bool> {
        let path = self.layout.credentials(context);
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Report::new(e)
                .change_context(credentials::Error::Delete(context.to_owned()))
                .attach(path.display().to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use remora_context::model::{Endpoint, Secret, Tls};

    use super::*;

    fn context(name: &str) -> Context {
        Context {
            name: name.into(),
            description: Some("test".into()),
            endpoint: Endpoint {
                address: "api.example:50051".into(),
                tls: Tls::default(),
            },
            roles: Default::default(),
            assumed_role: None,
            login: None,
        }
    }

    #[test]
    fn contexts_round_trip_and_list_sorted() {
        let root = tempfile::tempdir().unwrap();
        let store = FileContextStoreImpl::new(root.path());
        assert!(store.list().unwrap().is_empty());
        store.put(&context("zeta")).unwrap();
        store.put(&context("alpha")).unwrap();
        let names: Vec<_> = store.list().unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["alpha", "zeta"]);
        assert_eq!(store.get("alpha").unwrap(), Some(context("alpha")));
        store.delete("alpha").unwrap();
        store.delete("alpha").unwrap();
        assert_eq!(store.get("alpha").unwrap(), None);
    }

    #[test]
    fn current_context_survives_and_clears() {
        let root = tempfile::tempdir().unwrap();
        let store = FileContextStoreImpl::new(root.path());
        assert_eq!(store.current().unwrap(), None);
        store.set_current(Some("eu2")).unwrap();
        assert_eq!(store.current().unwrap().as_deref(), Some("eu2"));
        store.set_current(None).unwrap();
        assert_eq!(store.current().unwrap(), None);
    }

    #[test]
    fn credentials_are_private_and_removable() {
        let root = tempfile::tempdir().unwrap();
        let store = FileCredentialStoreImpl::new(root.path());
        let credentials = Credentials {
            secret: Secret::AccessKey {
                token: "s3cret".into(),
            },
        };
        assert!(store.get("eu2").unwrap().is_none());
        store.put("eu2", &credentials).unwrap();
        assert!(store.get("eu2").unwrap() == Some(credentials));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = root.path().join("contexts/eu2/credentials.json");
            let mode = fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        assert!(store.delete("eu2").unwrap());
        assert!(!store.delete("eu2").unwrap());
    }

    #[test]
    fn credentials_alone_are_not_a_context() {
        let root = tempfile::tempdir().unwrap();
        let credentials = FileCredentialStoreImpl::new(root.path());
        credentials
            .put(
                "ghost",
                &Credentials {
                    secret: Secret::AccessKey { token: "t".into() },
                },
            )
            .unwrap();
        assert!(FileContextStoreImpl::new(root.path())
            .list()
            .unwrap()
            .is_empty());
    }
}
