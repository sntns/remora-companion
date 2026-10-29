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
    /// The proxy a WebSocket goes through, from the environment as for the
    /// REST calls (reqwest reads it the same way); `None` to dial directly.
    proxy: Option<Url>,
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
        let host = dial
            .rsplit_once(':')
            .map_or(dial.as_str(), |(host, _)| host);
        let proxy = proxy_for(tls.is_some(), host, |name| std::env::var(name).ok());
        Ok(Self {
            http,
            base,
            dial,
            tls,
            headers,
            proxy,
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
    ///
    /// A refusal that says the call was not taken - 429, 503 - is tried
    /// again, after its `Retry-After` or a jittered backoff: many sessions
    /// opened at once, or a gateway restarting, otherwise failed outright.
    /// A request whose body is a stream cannot be sent twice, and is not.
    pub async fn execute(&self, request: RequestBuilder) -> std::result::Result<Response, Status> {
        let mut request = request;
        let mut attempt = 0;
        loop {
            let again = request.try_clone();
            let response = request
                .send()
                .await
                .map_err(|error| Status::unavailable(&error))?;
            match again {
                Some(again) if attempt < RETRIES && not_taken(response.status().as_u16()) => {
                    tokio::time::sleep(retry_wait(attempt, response.headers())).await;
                    request = again;
                    attempt += 1;
                }
                _ => return Ok(response),
            }
        }
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
    ///
    /// Retried like [`Gateway::execute`] on 429 and 503, and on no answer at
    /// all: nothing has happened before the upgrade, so trying again cannot
    /// do anything twice.
    pub async fn websocket(
        &self,
        url: Url,
        subprotocol: &str,
    ) -> std::result::Result<(WebSocket, HeaderMap), Status> {
        let mut attempt = 0;
        loop {
            match self.websocket_once(url.clone(), subprotocol).await {
                Err(refused) if attempt < RETRIES && refused.retry => {
                    tokio::time::sleep(refused.wait.unwrap_or_else(|| backoff(attempt))).await;
                    attempt += 1;
                }
                outcome => return outcome.map_err(|refused| refused.status),
            }
        }
    }

    async fn websocket_once(
        &self,
        url: Url,
        subprotocol: &str,
    ) -> std::result::Result<(WebSocket, HeaderMap), Refused> {
        let mut url = url;
        let scheme = if self.tls.is_some() { "wss" } else { "ws" };
        url.set_scheme(scheme).expect("http(s) becomes ws(s)");
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|error| Refused::final_(Status::new(Code::InvalidArgument, chain(&error))))?;
        request.headers_mut().extend(self.headers.clone());
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(subprotocol).expect("a subprotocol is a token"),
        );
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, self.connect())
            .await
            .map_err(|_| {
                Refused::retry(Status::new(
                    Code::Unavailable,
                    "the gateway did not answer in time".to_owned(),
                ))
            })??;
        // A keystroke is a small message: without it, Nagle holds it back
        // for the acknowledgement of the previous one.
        let _ = stream.set_nodelay(true);
        let connector = match &self.tls {
            Some(config) => Connector::Rustls(config.clone()),
            None => Connector::Plain,
        };
        let upgraded = tokio::time::timeout(
            UPGRADE_TIMEOUT,
            client_async_tls_with_config(request, stream, None, Some(connector)),
        )
        .await
        .map_err(|_| {
            Refused::final_(Status::new(
                Code::Unavailable,
                "the gateway did not open the channel in time".to_owned(),
            ))
        })?;
        match upgraded {
            Ok((socket, response)) => Ok((socket, response.headers().clone())),
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                let message = response
                    .body()
                    .as_deref()
                    .map(|body| String::from_utf8_lossy(body).trim().to_owned())
                    .unwrap_or_default();
                let http = response.status().as_u16();
                let status = Status::new(Code::from_http(http), message);
                Err(if not_taken(http) {
                    Refused {
                        status,
                        retry: true,
                        wait: retry_after(response.headers()),
                    }
                } else {
                    Refused::final_(status)
                })
            }
            Err(error) => Err(Refused::final_(Status::unavailable(&error))),
        }
    }

    /// A connection to the gateway: direct, or through the proxy's tunnel.
    async fn connect(&self) -> std::result::Result<TcpStream, Refused> {
        match &self.proxy {
            None => TcpStream::connect(&self.dial)
                .await
                .map_err(|error| Refused::retry(Status::unavailable(&error))),
            Some(proxy) => tunnel(proxy, &self.dial).await,
        }
    }
}

/// A WebSocket that did not open, and whether trying again can help.
struct Refused {
    status: Status,
    retry: bool,
    /// What the gateway asked to wait, if it said.
    wait: Option<Duration>,
}

impl Refused {
    fn retry(status: Status) -> Self {
        Self {
            status,
            retry: true,
            wait: None,
        }
    }

    fn final_(status: Status) -> Self {
        Self {
            status,
            retry: false,
            wait: None,
        }
    }
}

/// How many times a call the gateway did not take is tried again.
const RETRIES: u32 = 3;
/// The backoff's start and ceiling; and the longest `Retry-After` honoured,
/// so that a gateway cannot park a command for long.
const RETRY_BASE: Duration = Duration::from_millis(500);
const RETRY_CAP: Duration = Duration::from_secs(8);
const RETRY_AFTER_CAP: Duration = Duration::from_secs(30);

/// The two answers that say the call was not taken: rate limited, or the
/// service unavailable for now. Anything else may have happened, or will
/// not change.
fn not_taken(http: u16) -> bool {
    http == 429 || http == 503
}

/// `Retry-After` in seconds, the form the platform sends.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| Duration::from_secs(seconds).min(RETRY_AFTER_CAP))
}

fn retry_wait(attempt: u32, headers: &HeaderMap) -> Duration {
    retry_after(headers).unwrap_or_else(|| backoff(attempt))
}

/// Full jitter: anywhere up to the exponential bound, so that many clients
/// refused together do not come back together.
fn backoff(attempt: u32) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    let bound = RETRY_BASE
        .saturating_mul(1 << attempt.min(16))
        .min(RETRY_CAP);
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    bound.mul_f64((random % 1_000_000) as f64 / 1_000_000.0)
}

/// The proxy a connection to `host` goes through, read from the environment
/// the way curl and reqwest read it: `HTTPS_PROXY` for TLS (`HTTP_PROXY`
/// otherwise), then `ALL_PROXY`, each in either case, unless `NO_PROXY`
/// names the host. A proxy without a scheme is an `http://` one.
fn proxy_for(tls: bool, host: &str, env: impl Fn(&str) -> Option<String>) -> Option<Url> {
    let var = |name: &str| {
        env(name)
            .or_else(|| env(&name.to_lowercase()))
            .filter(|value| !value.trim().is_empty())
    };
    if let Some(excluded) = var("NO_PROXY") {
        if bypasses(host, &excluded) {
            return None;
        }
    }
    let value = if tls {
        var("HTTPS_PROXY")
    } else {
        var("HTTP_PROXY")
    }
    .or_else(|| var("ALL_PROXY"))?;
    let value = value.trim();
    let value = if value.contains("://") {
        value.to_owned()
    } else {
        format!("http://{value}")
    };
    Url::parse(&value).ok()
}

/// Whether `NO_PROXY` - comma-separated hosts, domains (with or without a
/// leading dot), or `*` - covers `host`.
fn bypasses(host: &str, excluded: &str) -> bool {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_lowercase();
    excluded.split(',').map(str::trim).any(|entry| {
        let entry = entry.trim_start_matches('.').to_lowercase();
        // A port is ignored; an IPv6 address has more than one colon.
        let entry = match entry.split_once(':') {
            Some((host, port)) if !port.contains(':') => host.to_owned(),
            _ => entry
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned(),
        };
        !entry.is_empty() && (entry == "*" || host == entry || host.ends_with(&format!(".{entry}")))
    })
}

/// A connection to `target` (`host:port`) through an HTTP proxy's CONNECT
/// tunnel, with its credentials when the proxy URL carries any. TLS to the
/// gateway then runs inside the tunnel, as it would on a direct connection.
async fn tunnel(proxy: &Url, target: &str) -> std::result::Result<TcpStream, Refused> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if proxy.scheme() != "http" {
        return Err(Refused::final_(Status::new(
            Code::InvalidArgument,
            format!(
                "a channel cannot go through a {}:// proxy; give an http:// one",
                proxy.scheme()
            ),
        )));
    }
    let address = format!(
        "{}:{}",
        proxy.host_str().unwrap_or_default(),
        proxy.port_or_known_default().unwrap_or(80)
    );
    let mut stream = TcpStream::connect(&address)
        .await
        .map_err(|error| Refused::retry(Status::unavailable(&error)))?;
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if !proxy.username().is_empty() {
        use base64::Engine;
        let credentials = format!(
            "{}:{}",
            percent_decoded(proxy.username()),
            percent_decoded(proxy.password().unwrap_or_default())
        );
        request.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64::engine::general_purpose::STANDARD.encode(credentials)
        ));
    }
    request.push_str("\r\n");
    let unavailable = |error: std::io::Error| Refused::retry(Status::unavailable(&error));
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(unavailable)?;

    // Read byte by byte up to the blank line: anything past it is the
    // gateway's, and must be left for TLS.
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err(Refused::final_(Status::new(
                Code::Unavailable,
                "the proxy's answer to CONNECT is too long".to_owned(),
            )));
        }
        let mut byte = [0u8; 1];
        if stream.read(&mut byte).await.map_err(unavailable)? == 0 {
            return Err(Refused::retry(Status::new(
                Code::Unavailable,
                "the proxy closed the connection during CONNECT".to_owned(),
            )));
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    let status_line = head.lines().next().unwrap_or_default().trim().to_owned();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok());
    match code {
        Some(200..=299) => Ok(stream),
        Some(code) if not_taken(code) || code == 502 || code == 504 => {
            Err(Refused::retry(Status::new(
                Code::Unavailable,
                format!("the proxy refused the tunnel: {status_line}"),
            )))
        }
        _ => Err(Refused::final_(Status::new(
            Code::Unavailable,
            format!("the proxy refused the tunnel: {status_line}"),
        ))),
    }
}

/// `%XX` escapes decoded, as a URL's user and password carry them.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(Ok(byte)) = text
                .get(i + 1..i + 3)
                .map(|hex| u8::from_str_radix(hex, 16))
            {
                decoded.push(byte);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// How long reaching the gateway may take.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// How long the upgrade may take: it answers once the device has decided,
/// which the platform bounds well under this.
const UPGRADE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn the_proxy_comes_from_the_environment_as_curl_reads_it() {
        let proxy = |tls, host, pairs| proxy_for(tls, host, env(pairs)).map(|url| url.to_string());
        assert_eq!(
            proxy(
                true,
                "api.eu2.sntns.io",
                &[("HTTPS_PROXY", "proxy.corp:3128")]
            ),
            Some("http://proxy.corp:3128/".into())
        );
        assert_eq!(
            proxy(
                true,
                "api.eu2.sntns.io",
                &[("https_proxy", "http://p:8080")]
            ),
            Some("http://p:8080/".into())
        );
        assert_eq!(
            proxy(false, "gateway", &[("HTTPS_PROXY", "http://p:8080")]),
            None,
            "HTTPS_PROXY is for TLS only"
        );
        assert_eq!(
            proxy(true, "api.eu2.sntns.io", &[("ALL_PROXY", "http://p:1")]),
            Some("http://p:1/".into())
        );
        assert_eq!(proxy(true, "api.eu2.sntns.io", &[]), None);
        assert_eq!(
            proxy(
                true,
                "api.eu2.sntns.io",
                &[
                    ("HTTPS_PROXY", "http://p:1"),
                    ("NO_PROXY", "localhost,.sntns.io")
                ]
            ),
            None
        );
    }

    #[test]
    fn no_proxy_names_hosts_domains_and_everything() {
        assert!(bypasses("api.eu2.sntns.io", "sntns.io"));
        assert!(bypasses("api.eu2.sntns.io", " .sntns.io "));
        assert!(bypasses("API.eu2.sntns.io", "api.eu2.sntns.io"));
        assert!(bypasses("anything", "*"));
        assert!(bypasses("[::1]", "::1"));
        assert!(!bypasses("api.eu2.sntns.io", "notsntns.io"));
        assert!(!bypasses("api.eu2.sntns.io", ""));
    }

    #[test]
    fn retry_after_is_honoured_up_to_a_bound() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert(reqwest::header::RETRY_AFTER, HeaderValue::from_static("2"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(2)));
        headers.insert(
            reqwest::header::RETRY_AFTER,
            HeaderValue::from_static("3600"),
        );
        assert_eq!(retry_after(&headers), Some(RETRY_AFTER_CAP));
    }

    #[test]
    fn backoff_stays_under_its_bound() {
        for attempt in 0..40 {
            assert!(backoff(attempt) <= RETRY_CAP);
        }
        assert!(backoff(0) <= RETRY_BASE);
    }

    #[test]
    fn proxy_credentials_are_percent_decoded() {
        assert_eq!(percent_decoded("p%40ss%3Aword"), "p@ss:word");
        assert_eq!(percent_decoded("100%"), "100%");
        assert_eq!(percent_decoded("plain"), "plain");
    }

    #[cfg(feature = "test-gateway")]
    mod served {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use axum::{
            extract::{ws::WebSocketUpgrade, State},
            response::{IntoResponse, Response},
            routing::get,
            Router,
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        use super::*;
        use crate::model::channel;
        use crate::test_gateway::{accept_channel, context, serve_router};
        use std::sync::Mutex;

        async fn gateway(address: String) -> Gateway {
            Gateway::new(&Connection::for_resolved(&context(address, "t")))
                .await
                .unwrap()
        }

        fn unavailable() -> Response {
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                [(axum::http::header::RETRY_AFTER, "0")],
                "busy",
            )
                .into_response()
        }

        /// Refused twice as not taken, then answered.
        async fn busy_then_ok(State(calls): State<Arc<AtomicUsize>>) -> Response {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                return unavailable();
            }
            axum::Json(serde_json::json!({"ok": true})).into_response()
        }

        async fn always_busy(State(calls): State<Arc<AtomicUsize>>) -> Response {
            calls.fetch_add(1, Ordering::SeqCst);
            unavailable()
        }

        async fn broken(State(calls): State<Arc<AtomicUsize>>) -> Response {
            calls.fetch_add(1, Ordering::SeqCst);
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "broken").into_response()
        }

        async fn busy_then_channel(
            State(calls): State<Arc<AtomicUsize>>,
            upgrade: WebSocketUpgrade,
        ) -> Response {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                return unavailable();
            }
            accept_channel(upgrade, "stream", "urn:device", |_socket| async {
                tokio::time::sleep(Duration::from_millis(200)).await;
            })
        }

        async fn serve(calls: Arc<AtomicUsize>) -> String {
            serve_router(
                Router::new()
                    .route("/busy", get(busy_then_ok))
                    .route("/always", get(always_busy))
                    .route("/broken", get(broken))
                    .route("/channel", get(busy_then_channel))
                    .with_state(calls),
            )
            .await
        }

        #[tokio::test]
        async fn a_call_the_gateway_did_not_take_is_tried_again() {
            let calls = Arc::new(AtomicUsize::new(0));
            let gateway = gateway(serve(calls.clone()).await).await;
            let answer: serde_json::Value = gateway.get(gateway.url(["busy"])).await.unwrap();
            assert_eq!(answer["ok"], true);
            assert_eq!(calls.load(Ordering::SeqCst), 3);
        }

        #[tokio::test]
        async fn retries_end() {
            let calls = Arc::new(AtomicUsize::new(0));
            let gateway = gateway(serve(calls.clone()).await).await;
            let status = gateway
                .get::<serde_json::Value>(gateway.url(["always"]))
                .await
                .unwrap_err();
            assert_eq!(status.code, Code::Unavailable);
            assert_eq!(calls.load(Ordering::SeqCst), 1 + RETRIES as usize);
        }

        #[tokio::test]
        async fn a_failure_that_may_have_happened_is_not_tried_again() {
            let calls = Arc::new(AtomicUsize::new(0));
            let gateway = gateway(serve(calls.clone()).await).await;
            assert!(gateway
                .get::<serde_json::Value>(gateway.url(["broken"]))
                .await
                .is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }

        #[tokio::test]
        async fn a_channel_the_gateway_did_not_take_is_tried_again() {
            let calls = Arc::new(AtomicUsize::new(0));
            let gateway = gateway(serve(calls.clone()).await).await;
            gateway
                .websocket(gateway.url(["channel"]), channel::SUBPROTOCOL)
                .await
                .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 3);
        }

        /// A CONNECT proxy that records what it was asked, then tunnels.
        async fn proxy() -> (String, Arc<Mutex<Vec<String>>>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let recorded = seen.clone();
            tokio::spawn(async move {
                loop {
                    let (mut client, _) = listener.accept().await.unwrap();
                    let recorded = recorded.clone();
                    tokio::spawn(async move {
                        let mut head = Vec::new();
                        while !head.ends_with(b"\r\n\r\n") {
                            let mut byte = [0u8; 1];
                            if client.read(&mut byte).await.unwrap() == 0 {
                                return;
                            }
                            head.push(byte[0]);
                        }
                        let head = String::from_utf8(head).unwrap();
                        recorded.lock().unwrap().push(head.clone());
                        let target = head.split_whitespace().nth(1).unwrap().to_owned();
                        let mut upstream = TcpStream::connect(target).await.unwrap();
                        client
                            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                            .await
                            .unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    });
                }
            });
            (address, seen)
        }

        #[tokio::test]
        async fn a_channel_goes_through_the_proxy() {
            let calls = Arc::new(AtomicUsize::new(2));
            let target = serve(calls).await;
            let (proxy, seen) = proxy().await;
            let mut gateway = gateway(target.clone()).await;
            gateway.proxy = Some(Url::parse(&format!("http://alice:p%40ss@{proxy}")).unwrap());
            gateway
                .websocket(gateway.url(["channel"]), channel::SUBPROTOCOL)
                .await
                .unwrap();
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert!(
                seen[0].starts_with(&format!("CONNECT {target} HTTP/1.1")),
                "{}",
                seen[0]
            );
            use base64::Engine;
            let credentials = base64::engine::general_purpose::STANDARD.encode("alice:p@ss");
            assert!(
                seen[0].contains(&format!("Proxy-Authorization: Basic {credentials}")),
                "{}",
                seen[0]
            );
        }

        #[tokio::test]
        async fn a_proxy_refusal_is_reported() {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap().to_string();
            tokio::spawn(async move {
                let (mut client, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 1024];
                let _ = client.read(&mut buffer).await;
                let _ = client
                    .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                    .await;
            });
            let mut gateway = gateway("127.0.0.1:9".into()).await;
            gateway.proxy = Some(Url::parse(&format!("http://{address}")).unwrap());
            let status = gateway
                .websocket(gateway.url(["channel"]), channel::SUBPROTOCOL)
                .await
                .unwrap_err();
            assert!(status.message.contains("407"), "{}", status.message);
        }
    }
}
