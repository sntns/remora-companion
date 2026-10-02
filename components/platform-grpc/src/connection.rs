use error_stack::{Report, ResultExt};
use tonic::{
    metadata::MetadataValue,
    service::{interceptor::InterceptedService, Interceptor},
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint},
    Request, Status,
};

use crate::error::{Error, Result};

/// Where a gateway is and how to reach it.
#[derive(Clone)]
pub struct Connection {
    /// `host:port`, e.g. `api.eu2.sntns.io:50051`. A scheme is accepted too
    /// and wins over [`Tls`] (`http://` is plaintext, `https://` is TLS).
    pub address: String,
    pub tls: Tls,
    pub credentials: Option<Credentials>,
}

#[derive(Debug, Clone, Default)]
pub struct Tls {
    /// Plaintext h2c, for a gateway on a local development stack only.
    pub disabled: bool,
    /// Extra PEM authorities to trust on top of the system and webpki roots.
    pub authorities: Vec<String>,
    /// The name to verify the server certificate against, when it is not
    /// the address's host.
    pub server_name: Option<String>,
}

impl Connection {
    /// How to reach `context`'s gateway, authenticated as `credentials`.
    pub fn for_context(
        context: &remora_context::model::Context,
        credentials: Option<&remora_context::model::Credentials>,
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
                let assume_role = credentials.assume_role.clone();
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
}

/// The platform's per-RPC credentials, sent as gRPC metadata on every call.
/// No `Debug`: a secret has no business in a log line.
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

/// Adds the platform's authentication metadata to every request, the way
/// sntns-service-go's `capability_client_grpc.go` does.
#[derive(Clone)]
pub struct AuthInterceptor {
    headers: Vec<(&'static str, MetadataValue<tonic::metadata::Ascii>)>,
}

impl AuthInterceptor {
    fn new(credentials: Option<&Credentials>) -> Result<Self> {
        let mut headers = Vec::new();
        let mut push = |key: &'static str, value: &str| -> Result<()> {
            let value = MetadataValue::try_from(value)
                .map_err(|_| Report::new(Error::InvalidCredentials))?;
            headers.push((key, value));
            Ok(())
        };
        let assume_role = match credentials {
            None => None,
            Some(Credentials::LoginProfile {
                identity,
                password,
                assume_role,
            }) => {
                push("authentication-identity", identity)?;
                push("authentication-password", password)?;
                assume_role.as_deref()
            }
            Some(Credentials::AccessKey { token, assume_role }) => {
                push("authentication-token", token)?;
                assume_role.as_deref()
            }
        };
        if let Some(role) = assume_role {
            push("assume-role", role)?;
        }
        Ok(Self { headers })
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> std::result::Result<Request<()>, Status> {
        for (key, value) in &self.headers {
            request.metadata_mut().insert(*key, value.clone());
        }
        Ok(request)
    }
}

/// A connected, authenticated channel to a gateway: hand it to any
/// generated client's `new`.
pub type GatewayChannel = InterceptedService<Channel, AuthInterceptor>;

/// Connects to a gateway now, so an unreachable or misconfigured address
/// fails here with a clear error rather than on the first call.
pub async fn connect(connection: &Connection) -> Result<GatewayChannel> {
    let interceptor = AuthInterceptor::new(connection.credentials.as_ref())?;

    let (uri, tls) = if connection.address.starts_with("http://") {
        (connection.address.clone(), false)
    } else if connection.address.starts_with("https://") {
        (connection.address.clone(), true)
    } else if connection.tls.disabled {
        (format!("http://{}", connection.address), false)
    } else {
        (format!("https://{}", connection.address), true)
    };

    let mut endpoint = Endpoint::from_shared(uri)
        .change_context_lazy(|| Error::InvalidAddress(connection.address.clone()))?;
    if tls {
        let mut config = ClientTlsConfig::new().with_enabled_roots();
        for authority in &connection.tls.authorities {
            config = config.ca_certificate(Certificate::from_pem(authority));
        }
        if let Some(name) = &connection.tls.server_name {
            config = config.domain_name(name.clone());
        }
        endpoint = endpoint.tls_config(config).change_context(Error::Tls)?;
    }

    let channel = endpoint
        .connect()
        .await
        .change_context_lazy(|| Error::Connect(connection.address.clone()))?;
    Ok(InterceptedService::new(channel, interceptor))
}
