//! From the one-directory-per-context layout (version 1) to version 2:
//! every context in `config.json`, every login's credentials in
//! `credentials.json`, and a context either a login, acting as itself, or a
//! role derived from one.
//!
//! In version 1 any context could assume a role, its own login's or one
//! it shared with a base (`login`), and role aliases lived in each
//! context. Each becomes:
//!
//! - a context without a role, its own login: a login, unchanged;
//! - a context sharing a base's login, with a role: derived from that base;
//! - a context with its own login *and* a role: derived from a login of the
//!   same identity on the same gateway when there is one -- its copy of
//!   the credentials goes -- else split in two, `<name>.login` keeping the
//!   login and `<name>` derived from it;
//! - a context sharing a base's login without a role: a login of its own,
//!   with a copy of the base's credentials (nothing to derive).
//!
//! A role given by alias becomes its URN; the aliases go. The previous
//! files are copied to `backup-v1/` first; `config.json`, written last, is
//! what commits the migration.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use error_stack::{Report, ResultExt};
use remora_context::model::{
    is_role_urn, short_role_name, Context, ContextKind, Contexts, Credentials, Derivation,
    Endpoint, Migration, Secret,
};
use serde::Deserialize;

use crate::{
    layout::{create_private_dir, read_optional, write_atomic, Layout},
    service::Config,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("failed to read {0}")]
    Read(PathBuf),
    #[error("failed to back up the contexts to {0}")]
    Backup(PathBuf),
    #[error("failed to write {0}")]
    Write(PathBuf),
}

/// A version-1 `meta.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Legacy {
    #[serde(default)]
    pub description: Option<String>,
    pub endpoint: Endpoint,
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
    #[serde(default)]
    pub assumed_role: Option<String>,
    /// The context whose login it shared.
    #[serde(default)]
    pub login: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyConfig {
    #[serde(default)]
    current_context: Option<String>,
}

/// What version 2 holds once migrated.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    pub contexts: Contexts,
    pub credentials: BTreeMap<String, Credentials>,
    pub changes: Vec<String>,
}

pub(crate) fn migrate(layout: &Layout, dry_run: bool) -> Result<Migration, Report<Error>> {
    let (current, legacy) = read(layout)?;
    let plan = plan(current, legacy);
    if dry_run {
        return Ok(Migration {
            backup: None,
            changes: plan.changes,
        });
    }

    let backup = layout.backup();
    back_up(layout, &backup).change_context_lazy(|| Error::Backup(backup.clone()))?;

    let path = layout.credentials();
    let bytes = serde_json::to_vec_pretty(&plan.credentials)
        .change_context_lazy(|| Error::Write(path.clone()))?;
    if !plan.credentials.is_empty() {
        write_atomic(&path, &bytes, true).change_context_lazy(|| Error::Write(path.clone()))?;
    }
    // The commit point: from here on, the store reads version 2.
    let path = layout.config();
    let bytes = serde_json::to_vec_pretty(&Config::from_contexts(&plan.contexts))
        .change_context_lazy(|| Error::Write(path.clone()))?;
    write_atomic(&path, &bytes, false).change_context_lazy(|| Error::Write(path.clone()))?;
    // Copied to the backup already; a failure leaves only a stray copy.
    let _ = fs::remove_dir_all(layout.legacy_contexts());

    Ok(Migration {
        backup: Some(backup),
        changes: plan.changes,
    })
}

type LegacyContexts = BTreeMap<String, (Legacy, Option<Credentials>)>;

fn read(layout: &Layout) -> Result<(Option<String>, LegacyContexts), Report<Error>> {
    let path = layout.config();
    let current = match read_optional(&path).change_context_lazy(|| Error::Read(path.clone()))? {
        None => None,
        Some(bytes) => {
            serde_json::from_slice::<LegacyConfig>(&bytes)
                .change_context_lazy(|| Error::Read(path.clone()))?
                .current_context
        }
    };

    let dir = layout.legacy_contexts();
    let mut contexts = BTreeMap::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((current, contexts)),
        Err(e) => return Err(Report::new(e).change_context(Error::Read(dir))),
    };
    for entry in entries {
        let entry = entry.change_context_lazy(|| Error::Read(dir.clone()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let meta = entry.path().join("meta.json");
        // A directory without meta.json (half-removed) was no context.
        let Some(bytes) = read_optional(&meta).change_context_lazy(|| Error::Read(meta.clone()))?
        else {
            continue;
        };
        let legacy: Legacy =
            serde_json::from_slice(&bytes).change_context_lazy(|| Error::Read(meta.clone()))?;
        let secret = entry.path().join("credentials.json");
        let credentials = read_optional(&secret)
            .change_context_lazy(|| Error::Read(secret.clone()))?
            .map(|bytes| serde_json::from_slice::<Credentials>(&bytes))
            .transpose()
            .change_context_lazy(|| Error::Read(secret.clone()))?;
        contexts.insert(name, (legacy, credentials));
    }
    Ok((current, contexts))
}

/// Whether two logins are the same identity: the same user, or the same
/// access key.
fn same_identity(a: &Credentials, b: &Credentials) -> bool {
    match (&a.secret, &b.secret) {
        (Secret::LoginProfile { identity: a, .. }, Secret::LoginProfile { identity: b, .. }) => {
            a == b
        }
        (Secret::AccessKey { token: a }, Secret::AccessKey { token: b }) => a == b,
        _ => false,
    }
}

/// The URN a version-1 role choice names: an alias of the context's roles,
/// or a URN as is. `None` for an alias it doesn't have.
fn role_urn(legacy: &Legacy) -> Option<Result<String, String>> {
    let choice = legacy.assumed_role.as_deref()?;
    Some(match legacy.roles.get(choice) {
        Some(urn) => Ok(urn.clone()),
        None if is_role_urn(choice) => Ok(choice.to_owned()),
        None => Err(choice.to_owned()),
    })
}

pub(crate) fn plan(current: Option<String>, legacy: LegacyContexts) -> Plan {
    let mut plan = Plan {
        contexts: Contexts {
            current,
            ..Default::default()
        },
        ..Default::default()
    };
    let taken = |plan: &Plan, name: &str| {
        legacy.contains_key(name) || plan.contexts.contexts.contains_key(name)
    };

    // The roles, by URN; an alias nothing defines can't be assumed (it
    // failed every command before too): that context acts as its login.
    let mut roles = BTreeMap::new();
    for (name, (context, _)) in &legacy {
        match role_urn(context) {
            Some(Ok(urn)) => {
                roles.insert(name.clone(), urn);
            }
            Some(Err(alias)) => plan.changes.push(format!(
                "{name}: role {alias:?} is no role it knows: it now acts as its login itself"
            )),
            None => {}
        }
    }

    // Logins first: every context with its own login and no role.
    for (name, (context, credentials)) in &legacy {
        if context.login.is_none() && !roles.contains_key(name) {
            plan.contexts.insert(Context {
                name: name.clone(),
                description: context.description.clone(),
                kind: ContextKind::Login {
                    endpoint: context.endpoint.clone(),
                },
            });
            if let Some(credentials) = credentials {
                plan.credentials.insert(name.clone(), credentials.clone());
            }
        }
    }

    // Then those with their own login and a role: derived from a login of
    // the same identity, or from their own login split off. `login_of`
    // says where each version-1 login now is, for the contexts that shared
    // it.
    let mut login_of: BTreeMap<String, String> = BTreeMap::new();
    for (name, (context, credentials)) in &legacy {
        let Some(role) = roles.get(name).filter(|_| context.login.is_none()) else {
            continue;
        };
        let same = credentials.as_ref().and_then(|own| {
            plan.contexts.contexts.values().find(|login| {
                login
                    .endpoint()
                    .is_some_and(|e| e.same_gateway(&context.endpoint))
                    && plan
                        .credentials
                        .get(&login.name)
                        .is_some_and(|theirs| same_identity(own, theirs))
            })
        });
        let base = match same {
            Some(login) => {
                plan.changes.push(format!(
                    "{name}: derives from {} as {}, whose login it duplicated",
                    login.name,
                    short_role_name(role)
                ));
                login.name.clone()
            }
            None => {
                let base = std::iter::once(format!("{name}.login"))
                    .chain((2..).map(|n| format!("{name}.login{n}")))
                    .find(|candidate| !taken(&plan, candidate))
                    .expect("some name is free");
                plan.contexts.insert(Context {
                    name: base.clone(),
                    description: Some(format!("{name}'s login")),
                    kind: ContextKind::Login {
                        endpoint: context.endpoint.clone(),
                    },
                });
                if let Some(credentials) = credentials {
                    plan.credentials.insert(base.clone(), credentials.clone());
                }
                plan.changes.push(format!(
                    "{name}: derives from {base} as {}, its login moved to {base}",
                    short_role_name(role)
                ));
                base
            }
        };
        login_of.insert(name.clone(), base.clone());
        plan.contexts.insert(Context {
            name: name.clone(),
            description: context.description.clone(),
            kind: ContextKind::Derived(Derivation {
                from: base,
                role: role.clone(),
                account: None,
            }),
        });
    }

    // Last, those that shared a base's login.
    for (name, (context, _)) in &legacy {
        let Some(shared) = &context.login else {
            continue;
        };
        let base = login_of.get(shared).unwrap_or(shared).clone();
        match roles.get(name) {
            Some(role)
                if plan
                    .contexts
                    .get(&base)
                    .is_some_and(|b| b.endpoint().is_some()) =>
            {
                plan.contexts.insert(Context {
                    name: name.clone(),
                    description: context.description.clone(),
                    kind: ContextKind::Derived(Derivation {
                        from: base,
                        role: role.clone(),
                        account: None,
                    }),
                });
            }
            role => {
                // No role, or a base that's gone: a login of its own, with
                // the base's credentials if it had any.
                plan.contexts.insert(Context {
                    name: name.clone(),
                    description: context.description.clone(),
                    kind: ContextKind::Login {
                        endpoint: context.endpoint.clone(),
                    },
                });
                let credentials = legacy
                    .get(shared)
                    .and_then(|(_, credentials)| credentials.clone());
                if let Some(credentials) = credentials {
                    plan.credentials.insert(name.clone(), credentials);
                }
                plan.changes.push(match role {
                    Some(_) => format!("{name}: {shared} is gone, so it is a login of its own"),
                    None => format!("{name}: a login of its own, with a copy of {shared}'s"),
                });
            }
        }
    }

    // A current context the previous layout lacked stays unset.
    if let Some(current) = &plan.contexts.current {
        if !plan.contexts.contexts.contains_key(current) {
            plan.contexts.current = None;
        }
    }
    plan
}

/// Copies the previous layout to `backup`, private: it holds credentials.
fn back_up(layout: &Layout, backup: &Path) -> io::Result<()> {
    create_private_dir(backup)?;
    let config = layout.config();
    if config.exists() {
        fs::copy(&config, backup.join("config.json"))?;
    }
    let contexts = layout.legacy_contexts();
    if contexts.exists() {
        copy_dir(&contexts, &backup.join("contexts"))?;
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    create_private_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            // fs::copy keeps the mode: credentials stay 0600.
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use remora_context::{
        adapter::{credentials::CredentialStoreAdapter, store::ContextStoreAdapter},
        model::Tls,
    };

    use super::*;
    use crate::{FileContextStoreImpl, FileCredentialStoreImpl};

    const DEV: &str = "urn:sntns:iam:eu2:a567:role:remora/support-admin";
    const PROD: &str = "urn:sntns:iam:eu2:0b7d:role:remora/support-admin";

    fn profile(identity: &str, password: &str) -> Credentials {
        Credentials {
            secret: Secret::LoginProfile {
                identity: identity.into(),
                password: password.into(),
            },
        }
    }

    /// Writes a version-1 context, as the previous binaries did.
    fn legacy(root: &Path, name: &str, meta: &str, credentials: Option<&Credentials>) {
        let dir = root.join("contexts").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("meta.json"), meta).unwrap();
        if let Some(credentials) = credentials {
            fs::write(
                dir.join("credentials.json"),
                serde_json::to_vec(credentials).unwrap(),
            )
            .unwrap();
        }
    }

    fn derived(contexts: &Contexts, name: &str) -> (String, String) {
        let derivation = contexts.get(name).unwrap().derivation().unwrap();
        (derivation.from.clone(), derivation.role.clone())
    }

    #[test]
    fn copies_of_a_login_with_a_role_derive_from_it() {
        // Today's layout of a real one: a login, and two hand-made copies
        // of it (the "name" inside still the original's) each assuming a
        // role of another tenant with its own copy of the credentials.
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let ada = profile("urn:sntns:iam:eu2:t:user:ada", "pw");
        fs::write(
            root.join("config.json"),
            r#"{"currentContext":"laplaylist-dev"}"#,
        )
        .unwrap();
        legacy(
            root,
            "laplaylist",
            r#"{"name":"laplaylist","endpoint":{"address":"https://api.eu2.sntns.io"}}"#,
            Some(&ada),
        );
        legacy(
            root,
            "laplaylist-dev",
            &format!(
                r#"{{"name":"laplaylist","endpoint":{{"address":"https://api.eu2.sntns.io"}},"assumedRole":"{DEV}"}}"#
            ),
            Some(&ada),
        );
        legacy(
            root,
            "laplaylist-prod",
            &format!(
                r#"{{"name":"laplaylist","endpoint":{{"address":"api.eu2.sntns.io"}},"assumedRole":"{PROD}"}}"#
            ),
            Some(&profile("urn:sntns:iam:eu2:t:user:ada", "older")),
        );
        legacy(
            root,
            "seniortech",
            r#"{"name":"seniortech","description":"mine","endpoint":{"address":"https://api.eu2.sntns.io"}}"#,
            Some(&profile("urn:sntns:iam:eu2:t:user:bob", "pw")),
        );

        let store = FileContextStoreImpl::new(root);
        let dry = store.migrate(true).unwrap().unwrap();
        assert_eq!(dry.backup, None);
        assert_eq!(dry.changes.len(), 2, "{:?}", dry.changes);
        assert!(root.join("contexts").exists(), "a dry run changed nothing");

        let done = store.migrate(false).unwrap().unwrap();
        assert_eq!(done.changes, dry.changes);
        let backup = done.backup.unwrap();
        assert!(backup.join("contexts/laplaylist-dev/meta.json").exists());
        assert!(!root.join("contexts").exists());
        assert_eq!(store.migrate(false).unwrap(), None);

        let contexts = store.load().unwrap();
        assert_eq!(contexts.current.as_deref(), Some("laplaylist-dev"));
        assert_eq!(contexts.contexts.len(), 4);
        assert_eq!(
            derived(&contexts, "laplaylist-dev"),
            ("laplaylist".into(), DEV.into())
        );
        assert_eq!(
            derived(&contexts, "laplaylist-prod"),
            ("laplaylist".into(), PROD.into())
        );
        assert_eq!(
            contexts.get("seniortech").unwrap().description.as_deref(),
            Some("mine")
        );

        let credentials = FileCredentialStoreImpl::new(root);
        assert!(credentials.get("laplaylist").unwrap() == Some(ada));
        assert!(credentials.get("laplaylist-dev").unwrap().is_none());
        assert!(credentials.get("seniortech").unwrap().is_some());
    }

    #[test]
    fn a_login_with_a_role_and_no_twin_is_split() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let key = Credentials {
            secret: Secret::AccessKey { token: "k".into() },
        };
        legacy(
            root,
            "acme",
            r#"{"endpoint":{"address":"api.example"},"roles":{"ops":"urn:x:role:ops"},"assumedRole":"ops"}"#,
            Some(&key),
        );
        // Taken already: the split picks the next name.
        legacy(
            root,
            "acme.login",
            r#"{"endpoint":{"address":"elsewhere"}}"#,
            None,
        );
        legacy(
            root,
            "acme-admin",
            r#"{"endpoint":{"address":"api.example"},"login":"acme","assumedRole":"urn:x:role:admin"}"#,
            None,
        );
        legacy(
            root,
            "acme-plain",
            r#"{"endpoint":{"address":"api.example"},"login":"acme"}"#,
            None,
        );

        let store = FileContextStoreImpl::new(root);
        // A load migrates on its own, silently.
        let contexts = store.load().unwrap();
        assert_eq!(
            derived(&contexts, "acme"),
            ("acme.login2".into(), "urn:x:role:ops".into())
        );
        assert_eq!(
            contexts.get("acme.login2").unwrap().endpoint(),
            Some(&Endpoint {
                address: "api.example".into(),
                tls: Tls::default()
            })
        );
        // Who shared acme's login derives from where that login went.
        assert_eq!(
            derived(&contexts, "acme-admin"),
            ("acme.login2".into(), "urn:x:role:admin".into())
        );
        // Sharing without a role: a login of its own, with a copy.
        assert!(contexts.get("acme-plain").unwrap().endpoint().is_some());
        let credentials = FileCredentialStoreImpl::new(root);
        assert!(credentials.get("acme-plain").unwrap() == Some(key.clone()));
        assert!(credentials.get("acme.login2").unwrap() == Some(key));
        assert!(credentials.get("acme").unwrap().is_none());
    }

    #[test]
    fn a_lone_config_file_is_migrated_too() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("config.json"), "{}").unwrap();
        let store = FileContextStoreImpl::new(root.path());
        assert!(store.load().unwrap().contexts.is_empty());
        let written = fs::read_to_string(root.path().join("config.json")).unwrap();
        assert!(written.contains("\"version\": 2"), "{written}");
    }
}
