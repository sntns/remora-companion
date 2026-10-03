use serde::{Deserialize, Serialize};

/// Who to be on a context's platform, as `rmra login` stored it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    #[serde(flatten)]
    pub secret: Secret,
}

/// The platform's two kinds of credentials -- the same two sntns-service-go
/// clients take (`login-profile`, `access-key`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Secret {
    /// A user's login profile: the user's URN and password.
    LoginProfile { identity: String, password: String },
    /// An access key's token.
    AccessKey { token: String },
}

impl Credentials {
    pub fn kind(&self) -> CredentialKind {
        match self.secret {
            Secret::LoginProfile { .. } => CredentialKind::LoginProfile,
            Secret::AccessKey { .. } => CredentialKind::AccessKey,
        }
    }
}

/// Never prints a secret, so a credential can't leak through a `{:?}` in
/// an error report or a log line.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Credentials");
        match &self.secret {
            Secret::LoginProfile { identity, .. } => debug
                .field("identity", identity)
                .field("password", &"<redacted>"),
            Secret::AccessKey { .. } => debug.field("token", &"<redacted>"),
        };
        debug.finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    LoginProfile,
    AccessKey,
}

impl std::fmt::Display for CredentialKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::LoginProfile => "login profile",
            Self::AccessKey => "access key",
        })
    }
}

/// Who the platform says the credentials belong to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub user_urn: String,
    pub user_name: String,
    /// The account's name, when the principal may read it.
    pub account_name: Option<String>,
}
