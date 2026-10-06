use std::future::Future;

use axum::{
    body::Bytes,
    extract::{rejection::BytesRejection, DefaultBodyLimit},
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use error_stack::{AttachmentKind, FrameKind, Report, ResultExt};
use remora_station::{
    application::{Error as StationError, StationService},
    model::{Ack, ClaimId},
};

use crate::{
    error::{Error, Result},
    wire::{AckBody, AckState, ClaimBody, ClaimStatusBody, ErrorBody, ErrorCode, HelloBody},
};

/// How long a device waits before retrying a claim the platform couldn't
/// take: long enough not to hammer a platform that's down.
const RETRY_AFTER_SECS: u64 = 5;

/// The largest request body: a claim is a P-256 CSR (under 1 KiB in
/// base64) and a few short fields, so anything near this is not a device.
/// Far below axum's 2 MB default, which anyone on the workshop network
/// could otherwise make the station parse.
pub const BODY_LIMIT: usize = 64 * 1024;

/// The protocol's routes over `service`. No logic beyond the wire: decoding
/// bodies (at most `BODY_LIMIT` bytes), and each station error to its
/// status code.
pub fn router(service: StationService) -> Router {
    Router::new()
        .route("/v1/hello", get(hello))
        .route("/v1/claims", post(claim))
        .route("/v1/claims/{claim_id}", get(status))
        .route("/v1/claims/{claim_id}/ack", post(ack))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
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

fn refuse(code: ErrorCode, error: String) -> Response {
    let status = StatusCode::from_u16(code.status()).expect("every code has a valid status");
    let retry_after = code.retries().then_some(RETRY_AFTER_SECS);
    let body = ErrorBody {
        error,
        code: Some(code),
        retry_after,
    };
    let mut response = (status, Json(body)).into_response();
    if let Some(seconds) = retry_after {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    }
    response
}

/// The headline, and the first detail under it (what the platform said,
/// else the innermost cause: which check a CSR failed, which field is
/// invalid): what a device's log needs.
fn message(report: &Report<StationError>) -> String {
    let headline = report.current_context().to_string();
    let attachment = report.frames().find_map(|frame| match frame.kind() {
        FrameKind::Attachment(AttachmentKind::Printable(printable)) => Some(printable.to_string()),
        _ => None,
    });
    let innermost = report
        .frames()
        .filter_map(|frame| match frame.kind() {
            FrameKind::Context(context) => Some(context.to_string()),
            FrameKind::Attachment(_) => None,
        })
        .last();
    match attachment.or(innermost) {
        Some(detail) if detail != headline => format!("{headline}: {detail}"),
        _ => headline,
    }
}

fn station_error(report: Report<StationError>) -> Response {
    let code = match report.current_context() {
        StationError::InvalidCsr => ErrorCode::InvalidCsr,
        StationError::InvalidHardware => ErrorCode::InvalidHardware,
        StationError::UnknownBoard(_) => ErrorCode::UnknownBoard,
        StationError::QuotaReached(_) => ErrorCode::QuotaExceeded,
        StationError::Refused => ErrorCode::Refused,
        StationError::MissingAccessUrl => ErrorCode::MissingAccessUrl,
        StationError::AlreadyExists(_) => ErrorCode::AlreadyExists,
        StationError::NotLabelled(_) => ErrorCode::NotLabelled,
        StationError::UnknownClaim(_) => ErrorCode::UnknownClaim,
        StationError::Unavailable => ErrorCode::Unavailable,
        StationError::Journal(_) => ErrorCode::JournalUnavailable,
        StationError::NotStarted => ErrorCode::NotStarted,
        _ => ErrorCode::Internal,
    };
    refuse(code, message(&report))
}

/// A JSON body; `Err` is the refusal to answer when it isn't one:
/// `invalid-request`, or `payload-too-large`.
fn body<T: serde::de::DeserializeOwned>(
    bytes: std::result::Result<Bytes, BytesRejection>,
) -> std::result::Result<T, (ErrorCode, String)> {
    let bytes = bytes.map_err(|rejection| {
        let code = if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ErrorCode::PayloadTooLarge
        } else {
            ErrorCode::InvalidRequest
        };
        (code, rejection.body_text())
    })?;
    serde_json::from_slice(&bytes).map_err(|e| {
        (
            ErrorCode::InvalidRequest,
            format!("malformed request body: {e}"),
        )
    })
}

async fn hello(State(service): State<StationService>) -> Response {
    match service.hello().await {
        Ok(hello) => Json(HelloBody::from(hello)).into_response(),
        Err(report) => station_error(report),
    }
}

async fn claim(
    State(service): State<StationService>,
    bytes: std::result::Result<Bytes, BytesRejection>,
) -> Response {
    let request = match body::<ClaimBody>(bytes) {
        Ok(claim) => match claim.into_request() {
            Ok(request) => request,
            Err(error) => return refuse(ErrorCode::InvalidRequest, error),
        },
        Err((code, error)) => return refuse(code, error),
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
    bytes: std::result::Result<Bytes, BytesRejection>,
) -> Response {
    let ack = match body::<AckBody>(bytes) {
        Ok(AckBody {
            state: AckState::Installed,
            ..
        }) => Ack::Installed,
        Ok(AckBody {
            state: AckState::Failed,
            reason,
        }) => Ack::Failed {
            reason: reason
                .filter(|reason| !reason.trim().is_empty())
                .unwrap_or_else(|| "no reason given".into()),
        },
        Err((code, error)) => return refuse(code, error),
    };
    match service.ack(&ClaimId::new(claim_id), ack).await {
        Ok(status) => Json(ClaimStatusBody::from(status)).into_response(),
        Err(report) => station_error(report),
    }
}
