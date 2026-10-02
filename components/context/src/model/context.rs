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

/// A context named for one invocation, overriding the stored current one.
#[derive(Debug, Clone)]
pub struct ContextOverride {
    pub name: String,
    pub source: Selection,
}

/// A context together with the credentials to use it: everything an
/// adapter needs to make an authenticated call.
#[derive(Debug, Clone)]
pub struct ResolvedContext {
    pub context: Context,
    pub credentials: Credentials,
    pub selection: Selection,
}
