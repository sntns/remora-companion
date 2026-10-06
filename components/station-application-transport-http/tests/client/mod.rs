//! A raw device side of the protocol for these tests: bodies as given,
//! malformed ones included (`remora-claim-adapter-http` is the real one).

use error_stack::{Report, ResultExt};
use remora_station_protocol::{
    AckBody, ClaimBody, ClaimStatusBody, ErrorBody, ErrorCode, HelloBody,
};
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to reach the station at {0}")]
    Unreachable(String),
    #[error("the station answered {status}: {message}")]
    Status {
        status: u16,
        code: Option<ErrorCode>,
        message: String,
        retry_after: Option<u64>,
    },
    #[error("unexpected answer from the station")]
    Parse,
}

pub type Result<T> = std::result::Result<T, Report<Error>>;

/// One method per route, a refusal as `Error::Status` with the station's
/// own message, code and, on a 503, how long to wait.
#[derive(Debug, Clone)]
pub struct StationClient {
    base: String,
    http: reqwest::Client,
}

impl StationClient {
    /// `base_url` like `http://10.42.0.1:8484`.
    pub fn new(base_url: &str) -> Self {
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn hello(&self) -> Result<HelloBody> {
        self.send(self.http.get(self.url("/v1/hello"))).await
    }

    pub async fn claim(&self, claim: &ClaimBody) -> Result<ClaimStatusBody> {
        self.send(self.http.post(self.url("/v1/claims")).json(claim))
            .await
    }

    pub async fn status(&self, claim_id: &str) -> Result<ClaimStatusBody> {
        self.send(self.http.get(self.url(&format!("/v1/claims/{claim_id}"))))
            .await
    }

    pub async fn ack(&self, claim_id: &str, ack: &AckBody) -> Result<ClaimStatusBody> {
        self.send(
            self.http
                .post(self.url(&format!("/v1/claims/{claim_id}/ack")))
                .json(ack),
        )
        .await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn send<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> Result<T> {
        let response = request
            .send()
            .await
            .change_context_lazy(|| Error::Unreachable(self.base.clone()))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .change_context_lazy(|| Error::Unreachable(self.base.clone()))?;
        if !status.is_success() {
            let refusal: Option<ErrorBody> = serde_json::from_slice(&bytes).ok();
            return Err(Report::new(Error::Status {
                status: status.as_u16(),
                code: refusal.as_ref().and_then(|body| body.code),
                message: refusal
                    .as_ref()
                    .map(|body| body.error.clone())
                    .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned()),
                retry_after: refusal.and_then(|body| body.retry_after),
            }));
        }
        serde_json::from_slice(&bytes).change_context(Error::Parse)
    }
}
