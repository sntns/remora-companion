use std::{sync::Arc, time::Duration};

use error_stack::{Report, ResultExt};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, SEC_WEBSOCKET_PROTOCOL},
    Method, RequestBuilder, Response,
};
use rustls::{pki_types::pem::PemObject, ClientConfig, RootCertStore};
use serde::{de::DeserializeOwned, Serialize};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    client_async_tls_with_config, tungstenite::client::IntoClientRequest, Connector,
    MaybeTlsStream, WebSocketStream,
};
use url::Url;

use crate::{
    connection::Connection,
    error::{chain, Code, Error, Result, Status},
};

/// A WebSocket opened on a gateway: a device channel, so far.
pub type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A gateway's REST API, authenticated: build URLs with [`Gateway::url`],
/// call with [`Gateway::get`] and friends (or [`Gateway::request`] and
/// [`Gateway::send`] for a route that is not JSON), open a WebSocket with
/// [`Gateway::websocket`]. Cheap to clone.
#[derive(Clone)]
pub struct Gateway {
    http: reqwest::Client,
    base: Url,
    /// `host:port` to connect to: the address's, even when the URL names
    /// the `server_name` override instead.
    dial: String,
    /// `None` for plain HTTP.
    tls: Option<Arc<ClientConfig>>,
    headers: HeaderMap,
}

/// A JSON error body: `google.rpc.Status`, as grpc-gateway renders it.
#[derive(serde::Deserialize)]
struct RpcStatus {
    code: i64,
    #[serde(default)]
    message: String,
}

impl Gateway {
    /// Sets up the client: nothing is sent yet, so an unreachable gateway
    /// is only found out by the first call (its [`Code::Unavailable`]).
    pub async fn new(connection: &Connection) -> Result<Self> {
        let mut base = connection.base_url()?;
        let dial = format!(
            "{}:{}",
            base.host_str().unwrap_or_default(),
            base.port_or_known_default().unwrap_or(443)
        );

        let mut headers = HeaderMap::new();
        for (name, value) in connection
            .credentials
            .iter()
            .flat_map(|credentials| credentials.headers())
        {
            let mut value =
                HeaderValue::from_str(value).change_context(Error::InvalidCredentials)?;
            value.set_sensitive(true);
            headers.insert(HeaderName::from_static(name), value);
        }

        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .user_agent(concat!("remora/", env!("CARGO_PKG_VERSION")));
        let tls = if base.scheme() == "https" {
            let config = Arc::new(tls_config(&connection.tls.authorities)?);
            builder = builder.use_preconfigured_tls((*config).clone());
            if let Some(name) = &connection.tls.server_name {
                let addresses: Vec<_> = tokio::net::lookup_host(&dial)
                    .await
                    .change_context_lazy(|| Error::Resolve(dial.clone()))?
                    .collect();
                base.set_host(Some(name))
                    .change_context_lazy(|| Error::InvalidAddress(name.clone()))?;
                builder = builder.resolve_to_addrs(name, &addresses);
            }
            Some(config)
        } else {
            None
        };
        let http = builder.build().change_context(Error::Client)?;
        Ok(Self {
            http,
            base,
            dial,
            tls,
            headers,
        })
    }

    /// The URL of a route, its segments percent-encoded: a name may hold
    /// anything a path segment cannot.
    pub fn url<S: AsRef<str>>(&self, segments: impl IntoIterator<Item = S>) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("a base URL has a host")
            .pop_if_empty()
            .extend(segments);
        url
    }

    /// A request carrying the credentials.
    pub fn request(&self, method: Method, url: Url) -> RequestBuilder {
        self.http.request(method, url).headers(self.headers.clone())
    }

    /// Sends a request; anything but a success comes back as the status
    /// the platform answered (or [`Code::Unavailable`] when it did not).
    pub async fn send(&self, request: RequestBuilder) -> std::result::Result<Response, Status> {
        let response = self.execute(request).await?;
        if response.status().is_success() {
            return Ok(response);
        }
        Err(Status::of(response).await)
    }

    /// Sends a request and hands back whatever the platform answered, for
    /// a caller that reads a refusal's headers too ([`Status::of`] then
    /// makes it a status); only no answer at all is an error here.
    pub async fn execute(&self, request: RequestBuilder) -> std::result::Result<Response, Status> {
        request
            .send()
            .await
            .map_err(|error| Status::unavailable(&error))
    }

    /// Sends a request and decodes its JSON answer.
    pub async fn call<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> std::result::Result<T, Status> {
        let body = self
            .send(request)
            .await?
            .bytes()
            .await
            .map_err(|error| Status::unavailable(&error))?;
        serde_json::from_slice(&body).map_err(Status::malformed)
    }

    pub async fn get<T: DeserializeOwned>(&self, url: Url) -> std::result::Result<T, Status> {
        self.call(self.request(Method::GET, url)).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, url: Url) -> std::result::Result<T, Status> {
        self.call(self.request(Method::DELETE, url)).await
    }

    pub async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        url: Url,
        body: &B,
    ) -> std::result::Result<T, Status> {
        self.call(self.request(Method::POST, url).json(body)).await
    }

    pub async fn patch<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        url: Url,
        body: &B,
    ) -> std::result::Result<T, Status> {
        self.call(self.request(Method::PATCH, url).json(body)).await
    }

    /// Opens a WebSocket on a route, offering `subprotocol`. A refusal
    /// comes before the upgrade, as an HTTP status with a plain-text
    /// reason; the upgrade's own headers come back with the socket.
    pub async fn websocket(
        &self,
        url: Url,
        subprotocol: &str,
    ) -> std::result::Result<(WebSocket, HeaderMap), Status> {
        let mut url = url;
        let scheme = if self.tls.is_some() { "wss" } else { "ws" };
        url.set_scheme(scheme).expect("http(s) becomes ws(s)");
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|error| Status::new(Code::InvalidArgument, chain(&error)))?;
        request.headers_mut().extend(self.headers.clone());
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(subprotocol).expect("a subprotocol is a token"),
        );
        let stream = TcpStream::connect(&self.dial)
            .await
            .map_err(|error| Status::unavailable(&error))?;
        let connector = match &self.tls {
            Some(config) => Connector::Rustls(config.clone()),
            None => Connector::Plain,
        };
        match client_async_tls_with_config(request, stream, None, Some(connector)).await {
            Ok((socket, response)) => Ok((socket, response.headers().clone())),
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                let message = response
                    .body()
                    .as_deref()
                    .map(|body| String::from_utf8_lossy(body).trim().to_owned())
                    .unwrap_or_default();
                Err(Status::new(
                    Code::from_http(response.status().as_u16()),
                    message,
                ))
            }
            Err(error) => Err(Status::unavailable(&error)),
        }
    }
}

impl Status {
    /// The status a failed response stands for: its JSON body's code when
    /// it has one (every generated route), else its HTTP status's, with
    /// the plain-text body as the message (the hand-written ones).
    pub async fn of(response: Response) -> Self {
        let http = response.status();
        let body = response.text().await.unwrap_or_default();
        if let Ok(status) = serde_json::from_str::<RpcStatus>(&body) {
            if status.code != 0 {
                return Self::new(Code::from_number(status.code), status.message);
            }
        }
        let message = body.trim();
        let message = if message.is_empty() {
            http.canonical_reason().unwrap_or_default()
        } else {
            message
        };
        Self::new(Code::from_http(http.as_u16()), message)
    }
}

/// The system's and Mozilla's roots, plus the context's own authorities.
fn tls_config(authorities: &[String]) -> Result<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // Best effort: a system store that is missing or partly unreadable
    // leaves the webpki roots.
    for certificate in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(certificate);
    }
    for authority in authorities {
        let mut added = 0;
        for certificate in rustls::pki_types::CertificateDer::pem_slice_iter(authority.as_bytes()) {
            roots
                .add(certificate.change_context(Error::Tls)?)
                .change_context(Error::Tls)?;
            added += 1;
        }
        if added == 0 {
            return Err(Report::new(Error::Tls).attach("an authority holds no PEM certificate"));
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .change_context(Error::Tls)?
        .with_root_certificates(roots)
        .with_no_client_auth())
}
