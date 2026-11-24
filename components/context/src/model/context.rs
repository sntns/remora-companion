use serde::{Deserialize, Serialize};

use super::credentials::{CredentialKind, Credentials};

/// A named way to act on a platform, like a `docker context`: either a
/// login of its own, or a role that another context's login assumes.
/// Credentials are not part of it -- they are stored apart (see
/// `CredentialStoreAdapter`), so that `rmra context inspect` can never
/// print a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub kind: ContextKind,
}

/// What a context is: a login, which always acts as itself, or a role
/// derived from one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContextKind {
    /// Logs in to `endpoint` with credentials of its own, as itself.
    Login { endpoint: Endpoint },
    /// Another context's login, assuming a role.
    Derived(Derivation),
}

/// A context derived from a login: nothing of the login is copied, its
/// endpoint and credentials are the login's at every call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Derivation {
    /// The login context: always a `Login`, never another derived one.
    pub from: String,
    /// The role every call assumes, a URN (`urn:…:role:…`), possibly of
    /// another tenant: how one login operates another account's devices.
    pub role: String,
    /// The account the role acts in, as the platform last said: for
    /// display only, never relied upon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

impl Context {
    /// A login context.
    pub fn login(name: impl Into<String>, endpoint: Endpoint) -> Self {
        Self {
            name: name.into(),
            description: None,
            kind: ContextKind::Login { endpoint },
        }
    }

    /// The name its login is stored under: its own, or the one it derives
    /// from.
    pub fn login_name(&self) -> &str {
        match &self.kind {
            ContextKind::Login { .. } => &self.name,
            ContextKind::Derived(derivation) => &derivation.from,
        }
    }

    pub fn derivation(&self) -> Option<&Derivation> {
        match &self.kind {
            ContextKind::Login { .. } => None,
            ContextKind::Derived(derivation) => Some(derivation),
        }
    }

    /// Its own endpoint: a login's. A derived context's is its login's.
    pub fn endpoint(&self) -> Option<&Endpoint> {
        match &self.kind {
            ContextKind::Login { endpoint } => Some(endpoint),
            ContextKind::Derived(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    /// The public gateway, `host[:port]` (e.g. `api.eu2.sntns.io`) or a
    /// base URL. A context from the gRPC days still says `:50051`: the
    /// same host serves the REST API on 443, which is what it reaches.
    pub address: String,
    #[serde(default, skip_serializing_if = "Tls::is_default")]
    pub tls: Tls,
}

impl Endpoint {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            tls: Tls::default(),
        }
    }

    /// Whether two endpoints reach the same gateway the same way, however
    /// their addresses are spelled: with or without a scheme, a trailing
    /// slash, or the gRPC days' `:50051` (or HTTPS's own `:443`).
    pub fn same_gateway(&self, other: &Endpoint) -> bool {
        self.tls == other.tls && normalize(&self.address) == normalize(&other.address)
    }
}

fn normalize(address: &str) -> String {
    let address = address.trim().to_ascii_lowercase();
    let address = address
        .strip_prefix("https://")
        .or_else(|| address.strip_prefix("http://"))
        .unwrap_or(&address);
    let address = address.trim_end_matches('/');
    address
        .strip_suffix(":50051")
        .or_else(|| address.strip_suffix(":443"))
        .unwrap_or(address)
        .to_owned()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tls {
    /// Plaintext, for a local development stack only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
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

/// Every stored context and the current one: what the store reads and
/// writes whole, so that a change touching several contexts (a rename, a
/// removal with the contexts derived from it) is one write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contexts {
    pub current: Option<String>,
    /// By name; each context's `name` is its key.
    pub contexts: std::collections::BTreeMap<String, Context>,
}

impl Contexts {
    pub fn get(&self, name: &str) -> Option<&Context> {
        self.contexts.get(name)
    }

    pub fn insert(&mut self, context: Context) {
        self.contexts.insert(context.name.clone(), context);
    }

    /// The contexts derived from `login`, by name.
    pub fn derived_from(&self, login: &str) -> Vec<String> {
        self.contexts
            .values()
            .filter(|context| context.derivation().is_some_and(|d| d.from == login))
            .map(|context| context.name.clone())
            .collect()
    }
}

/// What `context ls` shows per context.
#[derive(Debug, Clone)]
pub struct ContextSummary {
    pub context: Context,
    pub current: bool,
    /// How its login is logged in, if it is: its own, or the one it
    /// derives from.
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
    /// Defined whole by the environment (`RMRA_ADDRESS`, `RMRA_TOKEN`...),
    /// not stored: see [`DefinedContext`].
    Defined,
}

/// A context named for one invocation (`--context`, `RMRA_CONTEXT`), or
/// defined whole for it, overriding the stored current one. Only the
/// context: which role it acts as is the context's own, never a
/// per-command choice.
#[derive(Debug, Clone)]
pub struct ContextOverride {
    /// The context to use instead of the stored current one.
    pub name: String,
    pub source: Selection,
    /// The context itself, when the invocation defines it (`Defined`)
    /// rather than naming a stored one.
    pub defined: Option<DefinedContext>,
}

impl ContextOverride {
    /// A stored context, by name.
    pub fn named(name: impl Into<String>, source: Selection) -> Self {
        Self {
            name: name.into(),
            source,
            defined: None,
        }
    }

    /// A context defined for this invocation only.
    pub fn defined(defined: DefinedContext) -> Self {
        Self {
            name: defined.name.clone(),
            source: Selection::Defined,
            defined: Some(defined),
        }
    }
}

/// A context defined whole for one invocation, credentials included -- by
/// the environment, for an ephemeral container or a CI job: never stored,
/// so nothing of it outlives the process, and nothing about it can be
/// changed. A login and, optionally, the role it assumes (a URN).
#[derive(Debug, Clone)]
pub struct DefinedContext {
    pub name: String,
    pub endpoint: Endpoint,
    pub credentials: Credentials,
    pub role: Option<String>,
}

/// What `context update` changes; `None` leaves a setting as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextUpdate {
    /// An empty one removes it.
    pub description: Option<String>,
    /// A login's only.
    pub address: Option<String>,
    pub plaintext: Option<bool>,
    /// An empty one removes it.
    pub server_name: Option<String>,
    /// A derived context's only: verified with the platform first.
    pub role: Option<String>,
}

impl ContextUpdate {
    /// Whether it changes the endpoint, which only a login has.
    pub fn touches_endpoint(&self) -> bool {
        self.address.is_some() || self.plaintext.is_some() || self.server_name.is_some()
    }
}

/// Whether `text` reads as an IAM role URN (`urn:…:role:…`): the one rule
/// both the use case and a prompt check a role by.
pub fn is_role_urn(text: &str) -> bool {
    text.starts_with("urn:") && text.contains(":role:")
}

/// A role being assumed, and the account it acts in when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssumedRole {
    pub urn: String,
    pub account: Option<String>,
}

impl AssumedRole {
    /// The role's own name, past `:role:` (e.g. `remora/support-admin`).
    pub fn short_name(&self) -> &str {
        short_role_name(&self.urn)
    }

    /// `remora/support-admin @ acme`, or, while the account is unknown,
    /// with the tenant it is in (`remora/support-admin @ a567f09e…`).
    pub fn display_name(&self) -> String {
        describe_role(&self.urn, self.account.as_deref())
    }
}

/// How a role reads: its short name, `@` the account it acts in, or, while
/// that is unknown, the start of its tenant's id -- two tenants' roles of
/// the same name told apart either way.
pub fn describe_role(urn: &str, account: Option<&str>) -> String {
    let name = short_role_name(urn);
    match account {
        Some(account) => format!("{name} @ {account}"),
        None => match role_tenant(urn) {
            Some(tenant) if tenant.chars().count() > 8 => {
                format!("{name} @ {}…", tenant.chars().take(8).collect::<String>())
            }
            Some(tenant) => format!("{name} @ {tenant}"),
            None => name.to_owned(),
        },
    }
}

/// The tenant a role URN is in: the segment before `:role:`.
fn role_tenant(urn: &str) -> Option<&str> {
    let (head, _) = urn.split_once(":role:")?;
    head.rsplit(':').next().filter(|tenant| !tenant.is_empty())
}

/// A role URN's own name, past `:role:`; the URN itself if it has none.
pub fn short_role_name(urn: &str) -> &str {
    urn.split_once(":role:").map_or(urn, |(_, name)| name)
}

/// A context together with what it takes to use it: everything an adapter
/// needs to make an authenticated call.
#[derive(Debug, Clone)]
pub struct ResolvedContext {
    /// The context selected.
    pub name: String,
    /// The context whose login it uses: its own name, or its login's.
    pub login: String,
    /// Its login's endpoint.
    pub endpoint: Endpoint,
    pub credentials: Credentials,
    pub selection: Selection,
    /// The role every call assumes, if any.
    pub role: Option<AssumedRole>,
}

/// What a migration of the stored contexts did, or would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Migration {
    /// Where the previous files were kept, when anything was written.
    pub backup: Option<std::path::PathBuf>,
    /// One line per context changed, for the operator.
    pub changes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_context_reads_as_a_login_or_a_derived_one() {
        let login: Context =
            serde_json::from_str(r#"{"name":"eu2","endpoint":{"address":"api.eu2.sntns.io"}}"#)
                .unwrap();
        assert_eq!(
            login,
            Context::login("eu2", Endpoint::new("api.eu2.sntns.io"))
        );

        let derived: Context = serde_json::from_str(
            r#"{"name":"acme","from":"eu2","role":"urn:sntns:iam:eu2:t:role:ops"}"#,
        )
        .unwrap();
        assert_eq!(derived.login_name(), "eu2");
        assert_eq!(derived.endpoint(), None);
        let json = serde_json::to_string(&derived).unwrap();
        assert!(!json.contains("account"), "{json}");
    }

    #[test]
    fn gateways_compare_however_spelled() {
        let a = Endpoint::new("api.eu2.sntns.io");
        for same in [
            "https://api.eu2.sntns.io",
            "https://api.eu2.sntns.io/",
            "api.eu2.sntns.io:50051",
            "API.eu2.sntns.io:443",
        ] {
            assert!(a.same_gateway(&Endpoint::new(same)), "{same}");
        }
        assert!(!a.same_gateway(&Endpoint::new("api.sntns.dev")));
        let mut plain = Endpoint::new("api.eu2.sntns.io");
        plain.tls.disabled = true;
        assert!(!a.same_gateway(&plain));
    }

    #[test]
    fn roles_read_by_their_short_name() {
        let role = AssumedRole {
            urn: "urn:sntns:iam:eu2:0b7d:role:remora/support-admin".into(),
            account: Some("La Playlist".into()),
        };
        assert_eq!(role.display_name(), "remora/support-admin @ La Playlist");
        assert_eq!(short_role_name("not-a-role"), "not-a-role");
        assert_eq!(
            describe_role("urn:sntns:iam:eu2:a567f09e-beb0:role:ops", None),
            "ops @ a567f09e…"
        );
        assert_eq!(describe_role("urn:x:role:ops", None), "ops @ x");
    }
}
