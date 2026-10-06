use std::time::Duration;

use error_stack::{Report, ResultExt};
use remora_claim::adapter::station::{Error, Result, StationClientAdapter};
use remora_station::model::{Ack, ClaimId, ClaimRequest, ClaimStatus, Hello};
use remora_station_protocol::{AckBody, ClaimBody, ClaimStatusBody, ErrorBody, HelloBody};
use serde::de::DeserializeOwned;

/// How long connecting to a station may take: it is on the same workshop
/// network, so anything longer is a station that isn't there.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a whole request may take, a platform call included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The protocol over reqwest, with timeouts: a station gone silent is an
/// `Unreachable` to retry, never a device stuck waiting.
#[derive(Debug, Clone)]
pub struct HttpStationClientImpl {
    http: reqwest::Client,
}

impl HttpStationClientImpl {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("an HTTP client with timeouts builds"),
        }
    }
}

impl Default for HttpStationClientImpl {
    fn default() -> Self {
        Self::new()
    }
}

fn url(station: &str, path: &str) -> String {
    format!("{}{path}", station.trim_end_matches('/'))
}

/// A refusal, by what the device can do about it: wait (a 503 says how
/// long), or stop; the station's message and code are attached.
fn refusal(status: reqwest::StatusCode, bytes: &[u8]) -> Report<Error> {
    let body: Option<ErrorBody> = serde_json::from_slice(bytes).ok();
    let retry_after = body
        .as_ref()
        .and_then(|body| body.retry_after)
        .map(Duration::from_secs);
    let report = if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        Report::new(Error::Unavailable { retry_after })
    } else {
        Report::new(Error::Refused)
    };
    let said = match body {
        Some(ErrorBody {
            error,
            code: Some(code),
            ..
        }) => format!("{status} ({code:?}): {error}"),
        Some(ErrorBody { error, .. }) => format!("{status}: {error}"),
        None => format!("{status}: {}", String::from_utf8_lossy(bytes)),
    };
    report.attach(said)
}

impl HttpStationClientImpl {
    async fn send<T: DeserializeOwned>(
        &self,
        station: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<T> {
        let unreachable = || Error::Unreachable(station.to_string());
        let response = request.send().await.change_context_lazy(unreachable)?;
        let status = response.status();
        let bytes = response.bytes().await.change_context_lazy(unreachable)?;
        if !status.is_success() {
            return Err(refusal(status, &bytes));
        }
        serde_json::from_slice(&bytes).change_context(Error::Unexpected)
    }

    async fn status_of(
        &self,
        station: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<ClaimStatus> {
        self.send::<ClaimStatusBody>(station, request)
            .await?
            .into_status()
            .change_context(Error::Unexpected)
    }
}

#[async_trait::async_trait]
impl StationClientAdapter for HttpStationClientImpl {
    async fn hello(&self, station: &str) -> Result<Hello> {
        let hello: HelloBody = self
            .send(station, self.http.get(url(station, "/v1/hello")))
            .await
            .change_context_lazy(|| Error::NotAStation(station.to_string()))?;
        let answered = format!("it answered {} protocol {}", hello.service, hello.protocol);
        hello
            .into_hello()
            .ok_or_else(|| Report::new(Error::NotAStation(station.to_string())).attach(answered))
    }

    async fn claim(&self, station: &str, request: &ClaimRequest) -> Result<ClaimStatus> {
        let body = ClaimBody::from(request);
        self.status_of(
            station,
            self.http.post(url(station, "/v1/claims")).json(&body),
        )
        .await
    }

    async fn status(&self, station: &str, claim: &ClaimId) -> Result<ClaimStatus> {
        let route = url(station, &format!("/v1/claims/{claim}"));
        self.status_of(station, self.http.get(route)).await
    }

    async fn ack(&self, station: &str, claim: &ClaimId, ack: &Ack) -> Result<ClaimStatus> {
        let route = url(station, &format!("/v1/claims/{claim}/ack"));
        let body = AckBody::from(ack);
        self.status_of(station, self.http.post(route).json(&body))
            .await
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        http::StatusCode,
        routing::{get, post},
        Json, Router,
    };
    use remora_station::model::{HardwareInfo, ImageInfo};
    use remora_station_protocol::ErrorCode;

    use super::*;

    /// A station answering each route the one way the test needs.
    async fn station(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    fn refused(
        status: StatusCode,
        code: ErrorCode,
        retry_after: Option<u64>,
    ) -> (StatusCode, Json<ErrorBody>) {
        (
            status,
            Json(ErrorBody {
                error: "no".into(),
                code: Some(code),
                retry_after,
            }),
        )
    }

    fn request() -> ClaimRequest {
        ClaimRequest {
            csr_der: b"csr".to_vec(),
            hardware: HardwareInfo {
                board: "hub-v2".into(),
                ..Default::default()
            },
            image: ImageInfo::default(),
        }
    }

    #[tokio::test]
    async fn tells_a_station_from_anything_else() {
        let url = station(Router::new().route(
            "/v1/hello",
            get(|| async {
                Json(HelloBody {
                    service: "remora-station".into(),
                    protocol: 1,
                    environment: "eu2".into(),
                })
            }),
        ))
        .await;
        let hello = HttpStationClientImpl::new().hello(&url).await.unwrap();
        assert_eq!(hello.environment, "eu2");

        let other = station(Router::new().route(
            "/v1/hello",
            get(|| async {
                Json(HelloBody {
                    service: "something-else".into(),
                    protocol: 1,
                    environment: "eu2".into(),
                })
            }),
        ))
        .await;
        let report = HttpStationClientImpl::new()
            .hello(&other)
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NotAStation(_)));

        let nothing = station(Router::new()).await;
        let report = HttpStationClientImpl::new()
            .hello(&nothing)
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NotAStation(_)));
    }

    #[tokio::test]
    async fn sorts_refusals_into_waiting_and_giving_up() {
        let unavailable = station(Router::new().route(
            "/v1/claims",
            post(|| async {
                refused(
                    StatusCode::SERVICE_UNAVAILABLE,
                    ErrorCode::Unavailable,
                    Some(5),
                )
            }),
        ))
        .await;
        let report = HttpStationClientImpl::new()
            .claim(&unavailable, &request())
            .await
            .unwrap_err();
        assert!(matches!(
            report.current_context(),
            Error::Unavailable {
                retry_after: Some(retry)
            } if *retry == Duration::from_secs(5)
        ));

        let forbidden = station(Router::new().route(
            "/v1/claims",
            post(|| async { refused(StatusCode::FORBIDDEN, ErrorCode::UnknownBoard, None) }),
        ))
        .await;
        let report = HttpStationClientImpl::new()
            .claim(&forbidden, &request())
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Refused));
        assert!(format!("{report:?}").contains("UnknownBoard"), "{report:?}");

        // Nobody listening at all.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let report = HttpStationClientImpl::new()
            .claim(&url, &request())
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Unreachable(_)));
    }
}
