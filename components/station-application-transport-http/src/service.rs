use std::future::Future;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use error_stack::{Report, ResultExt};
use remora_station::{
    application::{Error as StationError, StationService},
    model::{Ack, ClaimId},
};

use crate::{
    error::{Error, Result},
    wire::{AckBody, AckState, ClaimBody, ClaimStatusBody, ErrorBody, HelloBody},
};

/// How long a device waits before retrying a claim the platform couldn't
/// take: long enough not to hammer a platform that's down.
const RETRY_AFTER_SECS: u64 = 5;

/// The protocol's routes over `service`. No logic beyond the wire: decoding
/// bodies, and each station error to its status code.
pub fn router(service: StationService) -> Router {
    Router::new()
        .route("/v1/hello", get(hello))
        .route("/v1/claims", post(claim))
        .route("/v1/claims/{claim_id}", get(status))
        .route("/v1/claims/{claim_id}/ack", post(ack))
        .with_state(service)
}

/// Serves `router(service)` on `listener` until `shutdown` completes.
pub async fn serve(
    listener: tokio::net::TcpListener,
    service: StationService,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    axum::serve(listener, router(service))
        .with_graceful_shutdown(shutdown)
        .await
        .change_context(Error::Serve)
}

fn refuse(status: StatusCode, error: String) -> Response {
    let retry_after = (status == StatusCode::SERVICE_UNAVAILABLE).then_some(RETRY_AFTER_SECS);
    let mut response = (status, Json(ErrorBody { error, retry_after })).into_response();
    if let Some(seconds) = retry_after {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    }
    response
}

/// The headline, and the first detail under it (why a CSR is invalid,
/// what the platform said): what a device's log needs.
fn message(report: &Report<StationError>) -> String {
    let headline = report.current_context().to_string();
    let detail = report.frames().find_map(|frame| {
        frame
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| frame.downcast_ref::<&str>().map(|s| s.to_string()))
    });
    match detail {
        Some(detail) if detail != headline => format!("{headline}: {detail}"),
        _ => headline,
    }
}

fn station_error(report: Report<StationError>) -> Response {
    let status = match report.current_context() {
        StationError::InvalidCsr | StationError::InvalidHardware(_) => StatusCode::BAD_REQUEST,
        StationError::UnknownBoard(_) | StationError::QuotaReached(_) | StationError::Refused => {
            StatusCode::FORBIDDEN
        }
        StationError::AlreadyExists(_) => StatusCode::CONFLICT,
        StationError::UnknownClaim(_) => StatusCode::NOT_FOUND,
        StationError::Unavailable | StationError::NotStarted => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    refuse(status, message(&report))
}

/// A JSON body; `Err` says what's malformed, for a 400.
fn body<T: serde::de::DeserializeOwned>(bytes: &Bytes) -> std::result::Result<T, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("malformed request body: {e}"))
}

async fn hello(State(service): State<StationService>) -> Response {
    match service.hello().await {
        Ok(hello) => Json(HelloBody::from(hello)).into_response(),
        Err(report) => station_error(report),
    }
}

async fn claim(State(service): State<StationService>, bytes: Bytes) -> Response {
    let request = match body::<ClaimBody>(&bytes).and_then(ClaimBody::into_request) {
        Ok(request) => request,
        Err(error) => return refuse(StatusCode::BAD_REQUEST, error),
    };
    match service.submit(request).await {
        Ok(status) => Json(ClaimStatusBody::from(status)).into_response(),
        Err(report) => station_error(report),
    }
}

async fn status(State(service): State<StationService>, Path(claim_id): Path<String>) -> Response {
    match service.status(&ClaimId::new(claim_id)).await {
        Ok(status) => Json(ClaimStatusBody::from(status)).into_response(),
        Err(report) => station_error(report),
    }
}

async fn ack(
    State(service): State<StationService>,
    Path(claim_id): Path<String>,
    bytes: Bytes,
) -> Response {
    let ack = match body::<AckBody>(&bytes) {
        Ok(AckBody {
            state: AckState::Installed,
            ..
        }) => Ack::Installed,
        Ok(AckBody {
            state: AckState::Failed,
            reason,
        }) => Ack::Failed {
            reason: reason
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| "no reason given".into()),
        },
        Err(error) => return refuse(StatusCode::BAD_REQUEST, error),
    };
    match service.ack(&ClaimId::new(claim_id), ack).await {
        Ok(status) => Json(ClaimStatusBody::from(status)).into_response(),
        Err(report) => station_error(report),
    }
}
