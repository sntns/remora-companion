//! The hub <-> station protocol v1, as it travels: plain HTTP/1.1, JSON,
//! binaries in standard base64 -- the bodies field for field, the error
//! codes, and their conversions to and from the station's model. One
//! place for both ends: the station's HTTP transport serves it, the claim
//! vertical's HTTP adapter speaks it (like `remora-platform-grpc`'s
//! protos for the platform). Nothing on it is secret -- a device checks
//! the identity it gets against anchors baked into its image -- so there
//! is no TLS.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD, Engine};
use remora_factory::adapter::provisioning::ProvisionedIdentity;
use remora_station::model::{
    Ack, ClaimId, ClaimRequest, ClaimState, ClaimStatus, HardwareInfo, Hello, ImageInfo, LabelState,
};
use serde::{Deserialize, Serialize};

/// `GET /v1/hello`'s `service`: what a device probing candidates looks for.
pub const SERVICE: &str = "remora-station";
pub const PROTOCOL: u32 = 1;
pub const DEFAULT_PORT: u16 = 8484;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloBody {
    pub service: String,
    pub protocol: u32,
    pub environment: String,
}

impl From<Hello> for HelloBody {
    fn from(hello: Hello) -> Self {
        Self {
            service: SERVICE.into(),
            protocol: PROTOCOL,
            environment: hello.environment,
        }
    }
}

impl HelloBody {
    /// The station's answer, when it is one speaking this protocol.
    pub fn into_hello(self) -> Option<Hello> {
        (self.service == SERVICE && self.protocol == PROTOCOL).then_some(Hello {
            environment: self.environment,
        })
    }
}

/// What makes a body unusable, beyond not being JSON.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BodyError {
    #[error("{field} is not standard base64: {source}")]
    Base64 {
        field: &'static str,
        source: base64::DecodeError,
    },
    #[error("hardware.board is missing")]
    MissingBoard,
}

fn decode(field: &'static str, value: &str) -> Result<Vec<u8>, BodyError> {
    STANDARD
        .decode(value.trim())
        .map_err(|source| BodyError::Base64 { field, source })
}

/// `POST /v1/claims`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimBody {
    /// Base64 DER PKCS#10, for a P-256 key.
    pub csr: String,
    pub hardware: HardwareBody,
    #[serde(default)]
    pub image: ImageBody,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HardwareBody {
    pub board: String,
    pub temp_hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eth_mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bsp_serial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub macs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatible: Option<String>,
}

impl ClaimBody {
    /// The request it carries. What it says about the device is the
    /// station's to check (`ClaimRequest::validate`).
    pub fn into_request(self) -> Result<ClaimRequest, BodyError> {
        let csr_der = decode("csr", &self.csr)?;
        if self.hardware.board.is_empty() {
            return Err(BodyError::MissingBoard);
        }
        let HardwareBody {
            board,
            temp_hostname,
            eth_mac,
            bsp_serial,
            machine_id,
            macs,
        } = self.hardware;
        // An empty field is an absent one (a hub sends "" for what it
        // can't read): templates and hooks treat them the same.
        let present = |field: Option<String>| field.filter(|value| !value.is_empty());
        Ok(ClaimRequest {
            csr_der,
            hardware: HardwareInfo {
                board,
                temp_hostname,
                eth_mac: present(eth_mac),
                bsp_serial: present(bsp_serial),
                machine_id: present(machine_id),
                macs: macs
                    .into_iter()
                    .filter(|(_, mac)| !mac.is_empty())
                    .collect(),
            },
            image: ImageInfo {
                version: present(self.image.version),
                compatible: present(self.image.compatible),
            },
        })
    }
}

impl From<&ClaimRequest> for ClaimBody {
    fn from(request: &ClaimRequest) -> Self {
        let hardware = &request.hardware;
        Self {
            csr: STANDARD.encode(&request.csr_der),
            hardware: HardwareBody {
                board: hardware.board.clone(),
                temp_hostname: hardware.temp_hostname.clone(),
                eth_mac: hardware.eth_mac.clone(),
                bsp_serial: hardware.bsp_serial.clone(),
                machine_id: hardware.machine_id.clone(),
                macs: hardware.macs.clone(),
            },
            image: ImageBody {
                version: request.image.version.clone(),
                compatible: request.image.compatible.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateBody {
    Issued,
    Installed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LabelBody {
    Queued,
    Active,
    Labelled,
}

/// Certificates in standard base64 DER.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityBody {
    pub serial_number: String,
    pub factory_device_name: String,
    pub certificate: String,
    pub certificate_authority: String,
    pub server_certificate_authority: String,
    /// The certificate reference's URN.
    pub key_id: String,
    pub access_url: String,
}

impl IdentityBody {
    pub fn into_identity(self) -> Result<ProvisionedIdentity, BodyError> {
        Ok(ProvisionedIdentity {
            certificate_der: decode("identity.certificate", &self.certificate)?,
            certificate_authority_der: decode(
                "identity.certificate_authority",
                &self.certificate_authority,
            )?,
            server_certificate_authority_der: decode(
                "identity.server_certificate_authority",
                &self.server_certificate_authority,
            )?,
            serial_number: self.serial_number,
            factory_device_name: self.factory_device_name,
            key_id: self.key_id,
            access_url: self.access_url,
        })
    }
}

/// `ClaimStatus`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimStatusBody {
    pub claim_id: String,
    pub state: StateBody,
    pub label: LabelBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<usize>,
    pub identity: IdentityBody,
}

impl From<ClaimStatus> for ClaimStatusBody {
    fn from(status: ClaimStatus) -> Self {
        let identity = status.identity;
        Self {
            claim_id: status.claim_id.to_string(),
            state: match status.state {
                ClaimState::Issued => StateBody::Issued,
                ClaimState::Installed => StateBody::Installed,
                ClaimState::Failed => StateBody::Failed,
            },
            label: match status.label {
                LabelState::Queued => LabelBody::Queued,
                LabelState::Active => LabelBody::Active,
                LabelState::Labelled => LabelBody::Labelled,
            },
            queue_position: status.queue_position,
            identity: IdentityBody {
                serial_number: identity.serial_number,
                factory_device_name: identity.factory_device_name,
                certificate: STANDARD.encode(identity.certificate_der),
                certificate_authority: STANDARD.encode(identity.certificate_authority_der),
                server_certificate_authority: STANDARD
                    .encode(identity.server_certificate_authority_der),
                key_id: identity.key_id,
                access_url: identity.access_url,
            },
        }
    }
}

impl ClaimStatusBody {
    pub fn claim_id(&self) -> ClaimId {
        ClaimId::new(self.claim_id.clone())
    }

    /// The status it carries.
    pub fn into_status(self) -> Result<ClaimStatus, BodyError> {
        Ok(ClaimStatus {
            claim_id: ClaimId::new(self.claim_id),
            state: match self.state {
                StateBody::Issued => ClaimState::Issued,
                StateBody::Installed => ClaimState::Installed,
                StateBody::Failed => ClaimState::Failed,
            },
            label: match self.label {
                LabelBody::Queued => LabelState::Queued,
                LabelBody::Active => LabelState::Active,
                LabelBody::Labelled => LabelState::Labelled,
            },
            queue_position: self.queue_position,
            identity: self.identity.into_identity()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AckState {
    Installed,
    Failed,
}

/// `POST /v1/claims/{claim_id}/ack`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckBody {
    pub state: AckState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl From<&Ack> for AckBody {
    fn from(ack: &Ack) -> Self {
        match ack {
            Ack::Installed => Self {
                state: AckState::Installed,
                reason: None,
            },
            Ack::Failed { reason } => Self {
                state: AckState::Failed,
                reason: Some(reason.clone()),
            },
        }
    }
}

impl AckBody {
    /// The ack it carries; a failure without a reason says so.
    pub fn into_ack(self) -> Ack {
        match self.state {
            AckState::Installed => Ack::Installed,
            AckState::Failed => Ack::Failed {
                reason: self
                    .reason
                    .filter(|reason| !reason.trim().is_empty())
                    .unwrap_or_else(|| "no reason given".into()),
            },
        }
    }
}

/// Every refusal: `{ "error", "code" }`, and `retry_after` (seconds) when
/// retrying later helps. `error` is for a log; `code` is what a device
/// decides on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    /// Absent only from a station predating codes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

/// Why the station refused, stable across versions (protocol v1): the
/// table of §3 in docs/specs/provisioning-station.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// 400: the CSR isn't a self-signed P-256 PKCS#10 -- the one refusal
    /// after which a device makes a new key.
    InvalidCsr,
    /// 400: a hardware or image field is refused (which one is in
    /// `error`), or a field the board's `device-name` needs is missing.
    InvalidHardware,
    /// 400: not JSON, a required field missing, bad base64.
    InvalidRequest,
    /// 413: a body over the station's limit.
    PayloadTooLarge,
    /// 403: the board isn't configured on this station.
    UnknownBoard,
    /// 403: this station's `max-claims` is reached.
    QuotaExceeded,
    /// 403: the platform refused (permission, policy).
    Refused,
    /// 403: the platform has no access URL for devices.
    MissingAccessUrl,
    /// 409: the explicit `device-name` already exists on the platform.
    AlreadyExists,
    /// 409: `installed` acked before the label was validated.
    NotLabelled,
    /// 404: no such claim on this station.
    UnknownClaim,
    /// 503: the platform can't be reached, or refuses the station's
    /// credentials; retry after `retry_after`.
    Unavailable,
    /// 503: the station's journal can't be written; retry after
    /// `retry_after`.
    JournalUnavailable,
    /// 503: the station isn't serving yet; retry after `retry_after`.
    NotStarted,
    /// 500: anything else.
    Internal,
}

impl ErrorCode {
    /// The HTTP status it comes with.
    pub fn status(self) -> u16 {
        match self {
            Self::InvalidCsr | Self::InvalidHardware | Self::InvalidRequest => 400,
            Self::PayloadTooLarge => 413,
            Self::UnknownBoard | Self::QuotaExceeded | Self::Refused | Self::MissingAccessUrl => {
                403
            }
            Self::AlreadyExists | Self::NotLabelled => 409,
            Self::UnknownClaim => 404,
            Self::Unavailable | Self::JournalUnavailable | Self::NotStarted => 503,
            Self::Internal => 500,
        }
    }

    /// Whether retrying the same request later can succeed with nobody
    /// changing anything on the device (`retry_after` is given).
    pub fn retries(self) -> bool {
        self.status() == 503
    }
}
