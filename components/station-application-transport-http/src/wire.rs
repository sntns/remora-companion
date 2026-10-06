//! The JSON bodies of protocol v1, field for field.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD, Engine};
use remora_factory::adapter::provisioning::ProvisionedIdentity;
use remora_station::model::{
    ClaimId, ClaimRequest, ClaimState, ClaimStatus, HardwareInfo, Hello, ImageInfo, LabelState,
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
    /// The request it carries; `Err` says what's malformed.
    pub fn into_request(self) -> Result<ClaimRequest, String> {
        let csr_der = STANDARD
            .decode(self.csr.trim())
            .map_err(|e| format!("csr is not standard base64: {e}"))?;
        if self.hardware.board.is_empty() {
            return Err("hardware.board is empty".into());
        }
        let HardwareBody {
            board,
            temp_hostname,
            eth_mac,
            bsp_serial,
            machine_id,
            macs,
        } = self.hardware;
        // An empty optional field is an absent one: templates and hooks
        // treat them the same.
        let present = |field: Option<String>| field.filter(|value| !value.is_empty());
        Ok(ClaimRequest {
            csr_der,
            hardware: HardwareInfo {
                board,
                temp_hostname,
                eth_mac: present(eth_mac),
                bsp_serial: present(bsp_serial),
                machine_id: present(machine_id),
                macs,
            },
            image: ImageInfo {
                version: present(self.image.version),
                compatible: present(self.image.compatible),
            },
        })
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
    pub fn into_identity(self) -> Result<ProvisionedIdentity, String> {
        let decode = |field: &str, value: &str| {
            STANDARD
                .decode(value)
                .map_err(|e| format!("identity.{field} is not standard base64: {e}"))
        };
        Ok(ProvisionedIdentity {
            certificate_der: decode("certificate", &self.certificate)?,
            certificate_authority_der: decode(
                "certificate_authority",
                &self.certificate_authority,
            )?,
            server_certificate_authority_der: decode(
                "server_certificate_authority",
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

/// Every refusal: `{ "error" }`, and `retry_after` (seconds) on a 503.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}
