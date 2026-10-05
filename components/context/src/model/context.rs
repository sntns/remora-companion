use serde::{Deserialize, Serialize};

use super::credentials::{CredentialKind, Credentials};

/// A named platform endpoint, like a `docker context`: which gateway to
/// talk to and how. Who to be there is not part of it -- that is the
/// context's credentials, stored separately (see `CredentialStoreAdapter`)
/// so that `rmra context inspect` can never print a secret and so that the
/// secrets can move to an OS keyring without touching contexts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub endpoint: Endpoint,
    /// IAM roles this context's login may assume, by alias -> role URN.
    /// A role may live in another tenant: assuming it is how one login
    /// operates another account's devices.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub roles: std::collections::BTreeMap<String, String>,
    /// The role every call assumes (`context role assume`): an alias of
    /// `roles`, or a role URN. None acts as the login itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assumed_role: Option<String>,
    /// The context whose login this one uses, for a context declined from a
    /// base one (`rmra context create --from`): one login, shared -- logging
    /// in or out of any of them does it for all. None: its own login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
}

impl Context {
    /// The name its login is stored under: its base's, or its own.
    pub fn login_name(&self) -> &str {
        self.login.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    /// The public gateway, `host:port` (e.g. `api.eu2.sntns.io:50051`).
    pub address: String,
    #[serde(default, skip_serializing_if = "Tls::is_default")]
    pub tls: Tls,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tls {
    /// Plaintext, for a local development stack only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Extra PEM certificate authorities to trust, on top of the system's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorities: Vec<String>,
    /// The name to verify the gateway's certificate against, when it is not
    /// the address's host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
}

impl Tls {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// What `context ls` shows per context.
#[derive(Debug, Clone)]
pub struct ContextSummary {
    pub context: Context,
    pub current: bool,
    /// How this context is logged in, if it is.
    pub credentials: Option<CredentialKind>,
}

/// How the context a command runs against was chosen, so a command can
/// say so ("using eu2, from RMRA_CONTEXT").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// `--context` on the command line.
    Flag,
    /// The `RMRA_CONTEXT` environment variable.
    Environment,
    /// The stored current context (`rmra context use`).
    Current,
    /// The only context there is.
    Only,
}

/// A context named for one invocation (`--context`, `RMRA_CONTEXT`),
/// overriding the stored current one. Only the context: which role it acts
/// as is the context's own setting, never a per-command choice.
#[derive(Debug, Clone)]
pub struct ContextOverride {
    /// The context to use instead of the stored current one.
    pub name: String,
    pub source: Selection,
}

/// The role a context is set to act as, when it is created or logged in
/// (`--assume-role`, `--no-assume-role`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RoleOverride {
    /// Whatever the context assumes, if anything.
    #[default]
    Keep,
    /// This role: an alias of the context's roles, or a role URN.
    Assume(String),
    /// None: act as the login itself.
    Drop,
}

/// A role being assumed: its URN, and the alias it was chosen by, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssumedRole {
    pub alias: Option<String>,
    pub urn: String,
}

impl AssumedRole {
    /// The alias when there is one, else the URN.
    pub fn display_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.urn)
    }
}

/// One of a context's roles, for `context role ls`.
#[derive(Debug, Clone)]
pub struct RoleSummary {
    pub alias: String,
    pub urn: String,
    /// Whether the context currently assumes it.
    pub assumed: bool,
}

/// A context together with the credentials to use it: everything an
/// adapter needs to make an authenticated call.
#[derive(Debug, Clone)]
pub struct ResolvedContext {
    pub context: Context,
    pub credentials: Credentials,
    pub selection: Selection,
    /// The role every call assumes, as the context says, if any.
    pub role: Option<AssumedRole>,
}
