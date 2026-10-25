//! The gateways' JSON, as grpc-gateway renders their protobuf messages:
//! lowerCamelCase names, every field present (`""`, `0`, `{}`, `[]`,
//! `null` for an unset message), 64-bit integers as strings, bytes as
//! base64, timestamps as RFC 3339. Only the routes and fields `rmra` and
//! `remora-etcher` use are here; names follow upstream's
//! (sntns-platform's `*-gateway-api-proto`). Every type reads and writes,
//! so a fake gateway speaks it too.

use std::{
    collections::HashMap,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

pub type Labels = HashMap<String, String>;

/// `sntns.service.v1.ResourceReference`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceReference {
    pub id: String,
    pub urn: String,
    pub name: String,
}

/// A `google.protobuf.Timestamp`'s JSON as a time.
pub fn time_of(timestamp: Option<&str>) -> Option<SystemTime> {
    humantime::parse_rfc3339_weak(timestamp?).ok()
}

/// A time as a `google.protobuf.Timestamp`'s JSON.
pub fn timestamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    humantime::format_rfc3339(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)).to_string()
}

/// int64 fields: a JSON string out, a string or a number in.
mod int64 {
    use serde::{de, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Int64 {
            Number(i64),
            Text(String),
            Null(()),
        }
        match Int64::deserialize(deserializer)? {
            Int64::Number(value) => Ok(value),
            Int64::Text(text) => text.parse().map_err(de::Error::custom),
            Int64::Null(()) => Ok(0),
        }
    }
}

/// bytes fields: standard base64 out; standard or URL-safe, padded or
/// not, in -- what protojson accepts.
mod bytes {
    use base64::{
        alphabet,
        engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
        Engine,
    };
    use serde::{de, Deserialize, Deserializer, Serializer};

    const LENIENT: GeneralPurposeConfig =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    const STANDARD: GeneralPurpose = GeneralPurpose::new(&alphabet::STANDARD, LENIENT);
    const URL_SAFE: GeneralPurpose = GeneralPurpose::new(&alphabet::URL_SAFE, LENIENT);

    pub fn serialize<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
        STANDARD
            .decode(&text)
            .or_else(|_| URL_SAFE.decode(&text))
            .map_err(de::Error::custom)
    }
}

// --- iam ---------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UserDescriptor {
    pub resource: Option<ResourceReference>,
    pub labels: Labels,
    pub has_login_profile: bool,
}

/// `GET /iam/v1/current-user`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GetCurrentUserResponse {
    pub user_descriptor: Option<UserDescriptor>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AccountDescriptor {
    pub resource: Option<ResourceReference>,
    pub labels: Labels,
}

/// `GET /iam/v1/current-account`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GetCurrentAccountResponse {
    pub account_descriptor: Option<AccountDescriptor>,
}

// --- remora: releases --------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReleaseArtifact {
    pub object_reference: Option<ResourceReference>,
    pub file_name: String,
    pub content_type: String,
    #[serde(with = "int64")]
    pub content_length: i64,
    pub checksum_sha256: String,
    pub tag_condition: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReleaseDescriptor {
    pub resource: Option<ResourceReference>,
    pub labels: Labels,
    pub version: String,
    pub artifacts: Vec<ReleaseArtifact>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReleaseSummary {
    pub resource: Option<ResourceReference>,
    pub version: String,
}

/// `POST /remora/v1/release`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CreateReleaseRequest {
    pub release_name: String,
    pub release_version: String,
    pub release_labels: Labels,
}

/// What creating, updating or deleting a release answers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReleaseReferenceResponse {
    pub release_reference: Option<ResourceReference>,
}

/// `GET /remora/v1/release/{release_name}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GetReleaseResponse {
    pub release_descriptor: Option<ReleaseDescriptor>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReleaseLabelsUpdate {
    pub set: Labels,
    pub merge: bool,
    pub unset: Vec<String>,
}

/// `PATCH /remora/v1/release/{release_name}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateReleaseRequest {
    pub release_labels: Option<ReleaseLabelsUpdate>,
}

/// `GET /remora/v1/release?releaseLabels[k]=v`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ListReleasesResponse {
    pub release_summaries: Vec<ReleaseSummary>,
}

/// `GET /remora/v1/release-artifact-upload-offset/{artifact_content_id}`:
/// how many bytes the platform holds for a content id (0 when unknown).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UploadOffsetResponse {
    #[serde(with = "int64")]
    pub artifact_content_offset: i64,
}

/// An artifact upload in as many short requests as it takes, the
/// hand-written routes under
/// `/remora/v1/release/{release_name}/artifact-upload/{artifact_content_id}`
/// (the content id is the client's own, kept for the whole upload):
///
/// - `PUT` with [`artifact_upload::OFFSET_HEADER`] (what is stored so far)
///   and the next bytes as a raw body: stored, never committed -- 204 with
///   the new offset, 409 with the real one when it was not that, 423 while
///   an earlier request still writes, 410 once committed. A request cut
///   short keeps what it delivered.
/// - `HEAD`: the offset, in the same header.
/// - `POST .../commit` with [`artifact_upload::CommitRequest`]: the
///   artifact, if what is stored has that length and checksum -- else 400,
///   and what was stored is dropped.
pub mod artifact_upload {
    use serde::{Deserialize, Serialize};

    use super::ResourceReference;

    /// How many bytes the platform holds: asked on a `PUT`, answered by
    /// every route.
    pub const OFFSET_HEADER: &str = "upload-offset";

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    #[serde(default)]
    pub struct CommitRequest {
        pub artifact_file_name: String,
        pub artifact_content_type: String,
        pub artifact_tag_condition: String,
        #[serde(with = "super::int64")]
        pub artifact_content_length: i64,
        /// SHA-256, hex.
        pub artifact_content_checksum: String,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    #[serde(default, rename_all = "camelCase")]
    pub struct CommitResponse {
        pub release_reference: Option<ResourceReference>,
    }
}

// --- remora: deployments -----------------------------------------------

/// `POST /remora/v1/deployment`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CreateDeploymentRequest {
    pub deployment_name: String,
    pub deployment_release_name: String,
    pub deployment_target_name: String,
    pub deployment_labels: Labels,
}

/// What creating, starting, cancelling or deleting a deployment answers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeploymentReferenceResponse {
    pub deployment_reference: Option<ResourceReference>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeploymentDescriptor {
    pub resource: Option<ResourceReference>,
    pub labels: Labels,
    pub release_reference: Option<ResourceReference>,
    pub target_reference: Option<ResourceReference>,
    pub status_value: String,
    pub status_details: Labels,
    pub status_timestamp: Option<String>,
}

/// `GET /remora/v1/deployment/{deployment_name}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GetDeploymentResponse {
    pub deployment_descriptor: Option<DeploymentDescriptor>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeploymentSummary {
    pub resource: Option<ResourceReference>,
    pub status_value: String,
}

/// `GET /remora/v1/deployment?deploymentTargetName=&deploymentReleaseName=
/// &deploymentLabels[k]=v`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ListDeploymentsResponse {
    pub deployment_summaries: Vec<DeploymentSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeploymentLogEntryProgress {
    #[serde(with = "int64")]
    pub current: i64,
    #[serde(with = "int64")]
    pub max: i64,
    pub unit: String,
    pub speed: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeploymentLogEntry {
    pub execution: String,
    pub result: String,
    pub code: i32,
    pub details: Vec<String>,
    pub recorded_at: Option<String>,
    pub progress: Option<DeploymentLogEntryProgress>,
}

/// `GET /remora/v1/deployment/{deployment_name}/log`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ListDeploymentLogEntriesResponse {
    pub deployment_log_entries: Vec<DeploymentLogEntry>,
}

/// A deployment's live feed, the WebSocket
/// `/remora/v1/deployment/{deployment_name}/log/stream`: text frames, the
/// current status first, then each new report and each status change as
/// they happen (reports already there when it opened are not replayed).
/// The gateway closes it normally once the deployment is terminal
/// ([`deployment_log::TERMINAL`]) or deleted ([`deployment_log::DELETED`]).
pub mod deployment_log {
    use serde::{Deserialize, Serialize};

    use super::{DeploymentLogEntry, Labels};

    /// The subprotocol to offer.
    pub const SUBPROTOCOL: &str = "sntns.remora.deployment-log.v1";
    /// The normal close's reason once the deployment is terminal.
    pub const TERMINAL: &str = "terminal";
    /// The normal close's reason once the deployment is gone.
    pub const DELETED: &str = "deleted";

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(
        tag = "type",
        rename_all = "lowercase",
        rename_all_fields = "camelCase"
    )]
    pub enum Frame {
        Status {
            #[serde(default)]
            status_value: String,
            #[serde(default)]
            status_details: Labels,
            #[serde(default)]
            status_timestamp: Option<String>,
        },
        Log {
            entry: DeploymentLogEntry,
        },
    }
}

// --- remora: devices ---------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeviceDescriptor {
    pub resource: Option<ResourceReference>,
    pub labels: Labels,
    pub certificates: Vec<ResourceReference>,
}

/// `GET /remora/v1/device/{device_name}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GetDeviceResponse {
    pub device_descriptor: Option<DeviceDescriptor>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeviceSummary {
    pub resource: Option<ResourceReference>,
}

/// `GET /remora/v1/device?deviceLabels[k]=v`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ListDevicesResponse {
    pub device_summaries: Vec<DeviceSummary>,
}

/// `POST /remora/v1/factory-device`: exactly one of `device_name` and
/// `serial_number_policy_name`, `force` only with a name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CreateFactoryDeviceRequest {
    pub device_name: String,
    /// DER.
    #[serde(with = "bytes")]
    pub certificate_signing_request: Vec<u8>,
    pub serial_number_policy_name: String,
    pub force: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CreateFactoryDeviceResponse {
    pub factory_device_name: String,
    pub certificate_reference: Option<ResourceReference>,
    /// DER, like both authorities.
    #[serde(with = "bytes")]
    pub certificate: Vec<u8>,
    #[serde(with = "bytes")]
    pub certificate_authority_certificate: Vec<u8>,
    #[serde(with = "bytes")]
    pub server_certificate_authority_certificate: Vec<u8>,
    pub access_url: String,
    pub serial_number: String,
}

/// `POST /remora/v1/factory-device/{device_name}/assignment`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AssignFactoryDeviceRequest {
    pub owner_account: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AssignFactoryDeviceResponse {
    pub factory_device_reference: Option<ResourceReference>,
    pub owner_reference: Option<ResourceReference>,
    #[serde(with = "bytes")]
    pub voucher: Vec<u8>,
}

// --- remora channel ----------------------------------------------------

/// The device channel's WebSocket,
/// `/remora/channel/v1/device/{channel_device_name}/channel/{channel_profile}`:
/// binary messages carry the bytes both ways, a text [`channel::EOF`] is a
/// half-close (either way), and everything the platform decides --
/// refusals included -- is decided before the upgrade.
pub mod channel {
    /// The subprotocol to offer.
    pub const SUBPROTOCOL: &str = "sntns.remora.channel.v1";
    /// The upgrade's answer: `stream` or `datagram`.
    pub const KIND_HEADER: &str = "sntns-channel-kind";
    /// The upgrade's answer: the device's URN.
    pub const DEVICE_HEADER: &str = "sntns-channel-device";
    /// The one text message: no more bytes this way.
    pub const EOF: &str = r#"{"type":"eof"}"#;
    /// The largest message the gateway reads off a stream channel; a
    /// datagram is at most 65535 bytes, one per message.
    pub const MAX_STREAM_MESSAGE: usize = 1024 * 1024;

    /// A text message: [`EOF`] is the only one there is.
    #[derive(Debug, serde::Deserialize)]
    pub struct Control {
        #[serde(rename = "type")]
        pub kind: String,
    }
}

/// `POST /remora/channel/v1/device/{channel_device_name}/ssh-certificate`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SignDeviceSshCertificateRequest {
    pub ssh_public_key: String,
    pub ssh_role: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SignDeviceSshCertificateResponse {
    pub ssh_certificate: String,
    pub ssh_user: String,
    pub ssh_host_key_alias: String,
    pub ssh_known_hosts: String,
}

/// `POST /remora/channel/v1/local-ssh-certificate`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SignDeviceLocalSshCertificateRequest {
    pub channel_device_names: Vec<String>,
    pub ssh_public_key: String,
    pub ssh_role: String,
    pub validity_hours: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LocalSshDevice {
    pub channel_device_name: String,
    pub ssh_user: String,
    pub ssh_host_key_alias: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SignDeviceLocalSshCertificateResponse {
    pub ssh_certificate: String,
    pub ssh_known_hosts: String,
    pub ssh_devices: Vec<LocalSshDevice>,
    pub valid_before: Option<String>,
}

/// `POST /remora/channel/v1/device/{channel_device_name}/login-code`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SignDeviceLoginChallengeRequest {
    pub ssh_role: String,
    pub challenge: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SignDeviceLoginChallengeResponse {
    pub login_code: String,
}

/// `POST /remora/channel/v1/device/{channel_device_name}/offline-login-codes`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IssueDeviceOfflineLoginCodesRequest {
    pub ssh_role: String,
    pub count: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OfflineLoginCode {
    pub index: u32,
    pub login_code: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IssueDeviceOfflineLoginCodesResponse {
    pub login_codes: Vec<OfflineLoginCode>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the gateway answers, verbatim from its encoding rules.
    #[test]
    fn reads_protojson() {
        let release: GetReleaseResponse = serde_json::from_str(
            r#"{"releaseDescriptor": {"resource": {"id": "1", "urn": "urn:r", "name": "r1"},
                "labels": {}, "version": "1.0",
                "artifacts": [{"objectReference": null, "fileName": "b.raucb",
                  "contentType": "", "contentLength": "12", "checksumSha256": "ab",
                  "tagCondition": "board:rp5"}]}}"#,
        )
        .unwrap();
        let descriptor = release.release_descriptor.unwrap();
        assert_eq!(descriptor.artifacts[0].content_length, 12);
        assert_eq!(descriptor.resource.unwrap().name, "r1");

        let created: CreateFactoryDeviceResponse = serde_json::from_str(
            r#"{"certificate": "aWRldmlk", "certificateAuthorityCertificate": "Y2E",
                "serverCertificateAuthorityCertificate": "",
                "certificateReference": null, "factoryDeviceName": "x"}"#,
        )
        .unwrap();
        assert_eq!(created.certificate, b"idevid");
        assert_eq!(created.certificate_authority_certificate, b"ca");
        assert!(created.server_certificate_authority_certificate.is_empty());

        let entry: DeploymentLogEntry = serde_json::from_str(
            r#"{"execution": "proceeding", "code": 0, "details": ["50%"],
                "recordedAt": "2026-10-25T12:34:56.123Z",
                "progress": {"current": "50", "max": 100, "unit": "%", "speed": 0}}"#,
        )
        .unwrap();
        assert_eq!(entry.progress.unwrap().current, 50);
        assert_eq!(
            time_of(entry.recorded_at.as_deref()),
            Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_792_931_696_123))
        );
    }

    #[test]
    fn writes_protojson() {
        let request = serde_json::to_value(CreateFactoryDeviceRequest {
            certificate_signing_request: b"csr".to_vec(),
            serial_number_policy_name: "hubs".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(request["certificateSigningRequest"], "Y3Ny");
        assert_eq!(request["serialNumberPolicyName"], "hubs");
        let offset = serde_json::to_value(UploadOffsetResponse {
            artifact_content_offset: 7,
        })
        .unwrap();
        assert_eq!(offset["artifactContentOffset"], "7");
        assert_eq!(
            timestamp(SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000)),
            "2026-09-21T14:13:20Z"
        );
        let frame: deployment_log::Frame = serde_json::from_str(
            r#"{"type":"status","statusValue":"RUNNING","statusDetails":{},"statusTimestamp":null}"#,
        )
        .unwrap();
        assert!(
            matches!(frame, deployment_log::Frame::Status { status_value, .. } if status_value == "RUNNING")
        );
        let frame: deployment_log::Frame = serde_json::from_str(
            r#"{"type":"log","entry":{"execution":"closed","result":"success","code":0,"details":[],"recordedAt":null,"progress":null}}"#,
        )
        .unwrap();
        assert!(matches!(frame, deployment_log::Frame::Log { entry } if entry.result == "success"));
        let commit = serde_json::to_value(artifact_upload::CommitRequest {
            artifact_file_name: "b.raucb".into(),
            artifact_content_length: 12,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(commit["artifact_file_name"], "b.raucb");
        assert_eq!(commit["artifact_content_length"], "12");
    }
}
