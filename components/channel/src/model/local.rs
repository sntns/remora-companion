use std::time::SystemTime;

use remora_context::model::ContextOverride;

use super::SshRole;

/// The platform's answer to a local certification request: one user
/// certificate for every device asked for, to log into their sshd directly
/// on their local network -- no channel, no platform in the path once
/// issued.
#[derive(Debug, Clone)]
pub struct LocalSshCertificate {
    /// The user certificate, OpenSSH format.
    pub certificate: String,
    /// `@cert-authority *<suffix> <host authority>`, a complete known_hosts
    /// line.
    pub known_hosts: String,
    /// Each device it was issued for, in the order asked.
    pub devices: Vec<LocalSshDevice>,
    /// When it stops being accepted.
    pub valid_before: Option<SystemTime>,
}

impl LocalSshCertificate {
    /// The entry for `device`, matched as the platform matches names:
    /// regardless of case.
    pub fn device(&self, device: &str) -> Option<&LocalSshDevice> {
        self.devices
            .iter()
            .find(|entry| entry.device.eq_ignore_ascii_case(device))
    }
}

/// How one device of a local certificate is logged into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSshDevice {
    /// The device's name (its serial).
    pub device: String,
    /// The device account the role logs into by default.
    pub user: String,
    /// The name the device's host certificate carries: ssh's HostKeyAlias,
    /// since the address it is reached at is not one the certificate names.
    pub host_key_alias: String,
}

/// Certify an operator's own key for direct ssh to devices on their local
/// network, for use with plain ssh.
pub struct LocalCertificateRequest {
    pub over: Option<ContextOverride>,
    /// 1 to 64 devices, by name. All or nothing: one the operator may not
    /// reach and nothing is certified.
    pub devices: Vec<String>,
    /// The key to certify, OpenSSH authorized-keys format.
    pub public_key: String,
    pub role: SshRole,
    /// How long it is valid for; `None` is the account's default.
    pub validity_hours: Option<u32>,
}

/// A console login code the device accepts once, without a challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineLoginCode {
    /// Its place in the device's series, which the device shows when it
    /// asks for one.
    pub index: u32,
    pub code: String,
}
