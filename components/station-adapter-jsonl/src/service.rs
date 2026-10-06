use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use error_stack::{Report, ResultExt};
use remora_factory::adapter::provisioning::ProvisionedIdentity;
use remora_station::{
    adapter::journal::{Error, JournalAdapter, JournalEntry, JournalEvent, Result},
    model::{BoardPolicy, ClaimId, DeviceNameTemplate, HardwareInfo, ImageInfo},
};
use serde::{Deserialize, Serialize};

/// The journal as JSON Lines: one object per transition, `ts` (RFC 3339,
/// UTC) and `claim_id` first, then `event` and its fields -- readable with
/// `jq`, appendable from anywhere, and the station's production register.
/// Each append is synced to disk before it returns; appends are serialized,
/// so concurrent ones never interleave within a line.
#[derive(Debug, Default, Clone)]
pub struct JsonlJournalImpl {
    appending: Arc<Mutex<()>>,
}

impl JsonlJournalImpl {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Serialize, Deserialize)]
struct Line {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ts: Option<String>,
    claim_id: String,
    #[serde(flatten)]
    event: Event,
}

/// Certificates in standard base64, like on the wire to the device.
#[derive(Serialize, Deserialize)]
struct Identity {
    serial_number: String,
    factory_device_name: String,
    certificate: String,
    certificate_authority: String,
    server_certificate_authority: String,
    key_id: String,
    access_url: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
enum Event {
    Received {
        board: String,
        temp_hostname: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        eth_mac: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bsp_serial: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine_id: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        macs: BTreeMap<String, String>,
        /// The image's version.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_compatible: Option<String>,
    },
    Issued {
        serial_number: String,
        factory_device_name: String,
        key_id: String,
        context: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serial_policy: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_name_template: Option<String>,
        identity: Identity,
    },
    Hook {
        hook: String,
        exit: Option<i32>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        timed_out: bool,
        attempt: u32,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        stdout: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        stderr: String,
    },
    LabelActive {
        attempt: u32,
    },
    Reprint {
        attempt: u32,
    },
    LabelMismatch {
        scanned: String,
    },
    LabelForced,
    LabelSkipped,
    LabelLost,
    Labelled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scanned: Option<String>,
    },
    Installed,
    Failed {
        reason: String,
    },
}

fn identity_line(identity: &ProvisionedIdentity) -> Identity {
    Identity {
        serial_number: identity.serial_number.clone(),
        factory_device_name: identity.factory_device_name.clone(),
        certificate: STANDARD.encode(&identity.certificate_der),
        certificate_authority: STANDARD.encode(&identity.certificate_authority_der),
        server_certificate_authority: STANDARD.encode(&identity.server_certificate_authority_der),
        key_id: identity.key_id.clone(),
        access_url: identity.access_url.clone(),
    }
}

fn identity_of(line: Identity) -> std::result::Result<ProvisionedIdentity, String> {
    let decode = |field: &str, value: &str| {
        STANDARD
            .decode(value)
            .map_err(|e| format!("identity.{field} is not base64: {e}"))
    };
    Ok(ProvisionedIdentity {
        certificate_der: decode("certificate", &line.certificate)?,
        certificate_authority_der: decode("certificate_authority", &line.certificate_authority)?,
        server_certificate_authority_der: decode(
            "server_certificate_authority",
            &line.server_certificate_authority,
        )?,
        serial_number: line.serial_number,
        factory_device_name: line.factory_device_name,
        key_id: line.key_id,
        access_url: line.access_url,
    })
}

fn to_line(entry: &JournalEntry, ts: SystemTime) -> Line {
    let event = match &entry.event {
        JournalEvent::Received { hardware, image } => Event::Received {
            board: hardware.board.clone(),
            temp_hostname: hardware.temp_hostname.clone(),
            eth_mac: hardware.eth_mac.clone(),
            bsp_serial: hardware.bsp_serial.clone(),
            machine_id: hardware.machine_id.clone(),
            macs: hardware.macs.clone(),
            image: image.version.clone(),
            image_compatible: image.compatible.clone(),
        },
        JournalEvent::Issued {
            identity,
            context,
            policy,
        } => {
            let (serial_policy, device_name_template) = match policy {
                BoardPolicy::SerialNumberPolicy(policy) => (Some(policy.clone()), None),
                BoardPolicy::DeviceName(template) => (None, Some(template.source().to_string())),
            };
            Event::Issued {
                serial_number: identity.serial_number.clone(),
                factory_device_name: identity.factory_device_name.clone(),
                key_id: identity.key_id.clone(),
                context: context.clone(),
                serial_policy,
                device_name_template,
                identity: identity_line(identity),
            }
        }
        JournalEvent::Hook {
            hook,
            exit,
            timed_out,
            attempt,
            stdout,
            stderr,
        } => Event::Hook {
            hook: hook.clone(),
            exit: *exit,
            timed_out: *timed_out,
            attempt: *attempt,
            stdout: stdout.clone(),
            stderr: stderr.clone(),
        },
        JournalEvent::LabelActive { attempt } => Event::LabelActive { attempt: *attempt },
        JournalEvent::Reprint { attempt } => Event::Reprint { attempt: *attempt },
        JournalEvent::LabelMismatch { scanned } => Event::LabelMismatch {
            scanned: scanned.clone(),
        },
        JournalEvent::LabelForced => Event::LabelForced,
        JournalEvent::LabelSkipped => Event::LabelSkipped,
        JournalEvent::LabelLost => Event::LabelLost,
        JournalEvent::Labelled { scanned } => Event::Labelled {
            scanned: scanned.clone(),
        },
        JournalEvent::Installed => Event::Installed,
        JournalEvent::Failed { reason } => Event::Failed {
            reason: reason.clone(),
        },
    };
    Line {
        ts: Some(humantime::format_rfc3339_millis(ts).to_string()),
        claim_id: entry.claim_id.to_string(),
        event,
    }
}

fn from_line(line: Line) -> std::result::Result<JournalEntry, String> {
    let event = match line.event {
        Event::Received {
            board,
            temp_hostname,
            eth_mac,
            bsp_serial,
            machine_id,
            macs,
            image,
            image_compatible,
        } => JournalEvent::Received {
            hardware: HardwareInfo {
                board,
                temp_hostname,
                eth_mac,
                bsp_serial,
                machine_id,
                macs,
            },
            image: ImageInfo {
                version: image,
                compatible: image_compatible,
            },
        },
        Event::Issued {
            context,
            serial_policy,
            device_name_template,
            identity,
            ..
        } => JournalEvent::Issued {
            identity: identity_of(identity)?,
            context,
            policy: match (serial_policy, device_name_template) {
                (Some(policy), None) => BoardPolicy::SerialNumberPolicy(policy),
                (None, Some(template)) => BoardPolicy::DeviceName(
                    DeviceNameTemplate::parse(&template).map_err(|e| e.to_string())?,
                ),
                _ => return Err("an issued entry names a serial policy xor a template".into()),
            },
        },
        Event::Hook {
            hook,
            exit,
            timed_out,
            attempt,
            stdout,
            stderr,
        } => JournalEvent::Hook {
            hook,
            exit,
            timed_out,
            attempt,
            stdout,
            stderr,
        },
        Event::LabelActive { attempt } => JournalEvent::LabelActive { attempt },
        Event::Reprint { attempt } => JournalEvent::Reprint { attempt },
        Event::LabelMismatch { scanned } => JournalEvent::LabelMismatch { scanned },
        Event::LabelForced => JournalEvent::LabelForced,
        Event::LabelSkipped => JournalEvent::LabelSkipped,
        Event::LabelLost => JournalEvent::LabelLost,
        Event::Labelled { scanned } => JournalEvent::Labelled { scanned },
        Event::Installed => JournalEvent::Installed,
        Event::Failed { reason } => JournalEvent::Failed { reason },
    };
    Ok(JournalEntry {
        claim_id: ClaimId::new(line.claim_id),
        event,
    })
}

/// Reads `path`; a last line without its newline is a write a crash cut
/// short: it is dropped, and cut from the file too, so the next append
/// starts on a line of its own.
fn load(path: &Path) -> Result<Vec<JournalEntry>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Report::new(error).change_context(Error::Read(path.to_path_buf())))
        }
    };
    let complete = match text.rfind('\n') {
        Some(end) => end + 1,
        None => 0,
    };
    if complete < text.len() {
        OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|file| file.set_len(complete as u64))
            .change_context_lazy(|| Error::Write(path.to_path_buf()))?;
    }
    let mut entries = Vec::new();
    for (index, line) in text[..complete].lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parse_error = || Error::Parse {
            path: path.to_path_buf(),
            line: index + 1,
        };
        let line: Line = serde_json::from_str(line).change_context_lazy(parse_error)?;
        let entry = from_line(line).map_err(|reason| Report::new(parse_error()).attach(reason))?;
        entries.push(entry);
    }
    Ok(entries)
}

fn append(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.sync_data()
}

#[async_trait::async_trait]
impl JournalAdapter for JsonlJournalImpl {
    async fn load(&self, path: &Path) -> Result<Vec<JournalEntry>> {
        let target = path.to_path_buf();
        tokio::task::spawn_blocking(move || load(&target))
            .await
            .change_context_lazy(|| Error::Read(path.to_path_buf()))?
    }

    async fn append(&self, path: &Path, entry: &JournalEntry) -> Result<()> {
        let mut line = serde_json::to_string(&to_line(entry, SystemTime::now()))
            .change_context_lazy(|| Error::Write(path.to_path_buf()))?;
        line.push('\n');
        let target: PathBuf = path.to_path_buf();
        let appending = Arc::clone(&self.appending);
        // fsync waits on the disk; keep it off the runtime's workers.
        tokio::task::spawn_blocking(move || {
            let _one_at_a_time = appending.lock().unwrap_or_else(|e| e.into_inner());
            append(&target, &line)
        })
        .await
        .change_context_lazy(|| Error::Write(path.to_path_buf()))?
        .change_context_lazy(|| Error::Write(path.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ProvisionedIdentity {
        ProvisionedIdentity {
            serial_number: "1H7Z".into(),
            factory_device_name: "urn:test:factory-device:1H7Z".into(),
            certificate_der: b"idevid".to_vec(),
            certificate_authority_der: b"factory ca".to_vec(),
            server_certificate_authority_der: b"server ca".to_vec(),
            key_id: "urn:test:certificate-1H7Z-1".into(),
            access_url: "https://access.test/access/v1".into(),
        }
    }

    fn entries() -> Vec<JournalEntry> {
        let id = ClaimId::new("9f3c");
        let entry = |event| JournalEntry {
            claim_id: id.clone(),
            event,
        };
        vec![
            entry(JournalEvent::Received {
                hardware: HardwareInfo {
                    board: "hub-v2".into(),
                    temp_hostname: "e2b4a1c09f13".into(),
                    eth_mac: Some("e2:b4:a1:c0:9f:13".into()),
                    bsp_serial: Some("c3d2".into()),
                    machine_id: None,
                    macs: BTreeMap::from([("wlan0".into(), "aa:bb".into())]),
                },
                image: ImageInfo {
                    version: Some("1.4.0".into()),
                    compatible: Some("v2".into()),
                },
            }),
            entry(JournalEvent::Issued {
                identity: identity(),
                context: "factory".into(),
                policy: BoardPolicy::DeviceName(DeviceNameTemplate::parse("{bsp_serial}").unwrap()),
            }),
            entry(JournalEvent::Hook {
                hook: "label.d/10-print".into(),
                exit: Some(0),
                timed_out: false,
                attempt: 1,
                stdout: "printed\n".into(),
                stderr: String::new(),
            }),
            entry(JournalEvent::LabelActive { attempt: 1 }),
            entry(JournalEvent::LabelMismatch {
                scanned: "1H8A".into(),
            }),
            entry(JournalEvent::Labelled {
                scanned: Some("1H7Z".into()),
            }),
            entry(JournalEvent::Installed),
        ]
    }

    #[tokio::test]
    async fn reloads_what_it_appended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("station.jsonl");
        let journal = JsonlJournalImpl::new();
        assert!(journal.load(&path).await.unwrap().is_empty());
        for entry in entries() {
            journal.append(&path, &entry).await.unwrap();
        }
        assert_eq!(journal.load(&path).await.unwrap(), entries());

        let text = fs::read_to_string(&path).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["event"], "received");
        assert_eq!(first["claim_id"], "9f3c");
        assert_eq!(first["image"], "1.4.0");
        assert!(first["ts"].as_str().unwrap().ends_with('Z'));
        let issued: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        assert_eq!(issued["serial_number"], "1H7Z");
        assert_eq!(
            issued["identity"]["certificate"],
            STANDARD.encode(b"idevid")
        );
        assert_eq!(issued["device_name_template"], "{bsp_serial}");
    }

    #[tokio::test]
    async fn drops_a_line_cut_short_and_refuses_a_corrupt_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("station.jsonl");
        let journal = JsonlJournalImpl::new();
        let all = entries();
        journal.append(&path, &all[0]).await.unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"ts\":\"2026-10-06T").unwrap();
        drop(file);

        assert_eq!(journal.load(&path).await.unwrap(), all[..1]);
        journal.append(&path, &all[1]).await.unwrap();
        assert_eq!(journal.load(&path).await.unwrap(), all[..2]);

        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"not json\n").unwrap();
        drop(file);
        let report = journal.load(&path).await.unwrap_err();
        assert!(matches!(
            report.current_context(),
            Error::Parse { line: 3, .. }
        ));
    }
}
