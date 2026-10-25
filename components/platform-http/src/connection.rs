use error_stack::{Report, ResultExt};
use url::Url;

use crate::error::{Error, Result};

/// The port contexts created for the platform's former gRPC API name: the
/// same host serves the REST API on HTTPS's own port, so a stored
/// `api.eu2.sntns.io:50051` keeps working without being edited.
const LEGACY_GRPC_PORT: u16 = 50051;

/// Where a gateway is and how to reach it.
#[derive(Clone)]
pub struct Connection {
    /// `host[:port]`, e.g. `api.eu2.sntns.io`, or a base URL. A scheme
    /// wins over [`Tls`] (`http://` is plaintext, `https://` is TLS).
    pub address: String,
    pub tls: Tls,
    pub credentials: Option<Credentials>,
}

#[derive(Debug, Clone, Default)]
pub struct Tls {
    /// Plain HTTP, for a gateway on a local development stack only.
    pub disabled: bool,
    /// Extra PEM authorities to trust on top of the system and webpki roots.
    pub authorities: Vec<String>,
    /// The name to verify the server certificate against, when it is not
    /// the address's host. It is the name requested too (the platform's
    /// ingress routes by `Host`); the address is still where to connect.
    pub server_name: Option<String>,
}

impl Connection {
    /// How to reach a resolved context's gateway, as its login, assuming
    /// the role the invocation resolved to (if any): what every adapter
    /// acting for the operator uses.
    pub fn for_resolved(resolved: &remora_context::model::ResolvedContext) -> Self {
        Self::for_context(
            &resolved.context,
            Some(&resolved.credentials),
            resolved.role.as_ref().map(|role| role.urn.as_str()),
        )
    }

    /// How to reach `context`'s gateway, authenticated as `credentials`,
    /// assuming `role` (a role URN) when given.
    pub fn for_context(
        context: &remora_context::model::Context,
        credentials: Option<&remora_context::model::Credentials>,
        role: Option<&str>,
    ) -> Self {
        use remora_context::model::Secret;
        let tls = &context.endpoint.tls;
        Self {
            address: context.endpoint.address.clone(),
            tls: Tls {
                disabled: tls.disabled,
                authorities: tls.authorities.clone(),
                server_name: tls.server_name.clone(),
            },
            credentials: credentials.map(|credentials| {
                let assume_role = role.map(str::to_owned);
                match &credentials.secret {
                    Secret::LoginProfile { identity, password } => Credentials::LoginProfile {
                        identity: identity.clone(),
                        password: password.clone(),
                        assume_role,
                    },
                    Secret::AccessKey { token } => Credentials::AccessKey {
                        token: token.clone(),
                        assume_role,
                    },
                }
            }),
        }
    }

    /// The REST API's base URL: the address, with its scheme (`https`
    /// unless TLS is disabled) and without the former gRPC port.
    pub fn base_url(&self) -> Result<Url> {
        let invalid = || Error::InvalidAddress(self.address.clone());
        let explicit = ["http://", "https://"]
            .iter()
            .any(|scheme| self.address.starts_with(scheme));
        let address = match self.address.trim_end_matches('/') {
            trimmed if trimmed.ends_with(':') => self.address.as_str(),
            trimmed => trimmed,
        };
        let mut url = if explicit {
            Url::parse(address)
        } else {
            let scheme = if self.tls.disabled { "http" } else { "https" };
            Url::parse(&format!("{scheme}://{address}"))
        }
        .change_context_lazy(invalid)?;
        if url.host_str().is_none() || url.query().is_some() || url.fragment().is_some() {
            return Err(Report::new(invalid()));
        }
        if !explicit && url.scheme() == "https" && url.port() == Some(LEGACY_GRPC_PORT) {
            let _ = url.set_port(None);
        }
        Ok(url)
    }
}

/// The platform's credentials, sent as headers on every request (the
/// WebSocket upgrades included) -- the ones its gRPC API took as metadata,
/// which every REST route reads too. No `Debug`: a secret has no business
/// in a log line.
#[derive(Clone)]
pub enum Credentials {
    /// A user's login profile: its URN and password.
    LoginProfile {
        identity: String,
        password: String,
        assume_role: Option<String>,
    },
    /// An access key's token.
    AccessKey {
        token: String,
        assume_role: Option<String>,
    },
}

impl Credentials {
    /// The headers, the way sntns-service-go's access sidecar reads them.
    pub(crate) fn headers(&self) -> Vec<(&'static str, &str)> {
        let (mut headers, assume_role) = match self {
            Self::LoginProfile {
                identity,
                password,
                assume_role,
            } => (
                vec![
                    ("authentication-identity", identity.as_str()),
                    ("authentication-password", password.as_str()),
                ],
                assume_role,
            ),
            Self::AccessKey { token, assume_role } => {
                (vec![("authentication-token", token.as_str())], assume_role)
            }
        };
        if let Some(role) = assume_role {
            headers.push(("assume-role", role.as_str()));
        }
        headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(address: &str, disabled: bool) -> String {
        Connection {
            address: address.into(),
            tls: Tls {
                disabled,
                ..Tls::default()
            },
            credentials: None,
        }
        .base_url()
        .map(|url| url.to_string())
        .unwrap_or_else(|report| format!("error: {report}"))
    }

    #[test]
    fn an_address_becomes_a_base_url() {
        assert_eq!(base("api.eu2.sntns.io", false), "https://api.eu2.sntns.io/");
        // A context made for the gRPC API: the same host, on 443.
        assert_eq!(
            base("api.eu2.sntns.io:50051", false),
            "https://api.eu2.sntns.io/"
        );
        assert_eq!(base("gateway:8443", false), "https://gateway:8443/");
        assert_eq!(base("127.0.0.1:8888", true), "http://127.0.0.1:8888/");
        // A local stack serves plain HTTP wherever it says.
        assert_eq!(base("localhost:50051", true), "http://localhost:50051/");
        // An explicit scheme wins, port included.
        assert_eq!(base("http://gateway:50051", false), "http://gateway:50051/");
        assert_eq!(
            base("https://gateway.example/api/", true),
            "https://gateway.example/api"
        );
        assert_eq!(base("[::1]:8888", true), "http://[::1]:8888/");
        assert!(base("", false).starts_with("error"));
        assert!(base("https://", false).starts_with("error"));
        assert!(base("gateway?x=1", false).starts_with("error"));
    }
}
