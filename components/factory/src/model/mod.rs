/// A device's private key, PKCS#8 DER. Generated locally and never sent
/// anywhere -- only the CSR built from it crosses the wire. Opaque, and
/// redacted in `Debug`: error-stack reports print with `{:?}`, and this key
/// is the device's identity.
#[derive(Clone)]
pub struct PrivateKey(Vec<u8>);

impl PrivateKey {
    pub fn from_pkcs8_der(der: Vec<u8>) -> Self {
        Self(der)
    }

    pub fn pkcs8_der(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateKey(<redacted>)")
    }
}

/// Everything a manufactured device needs to self-enroll at first boot,
/// per `sntns-platform`'s device-bootstrap contract: what the platform's
/// factory-device call hands back, plus the private key generated locally
/// to request it (the platform never sees this key -- only the CSR built
/// from it crosses the wire).
///
/// Written out as `remora-factory.yaml` by a `CredentialWriterAdapter`
/// (PEM encoding/SEC1 conversion are format concerns, not domain data --
/// see `remora-factory-adapter-local`), matching `remora-edge`'s actual
/// `FactoryIdentity` parser: `url`, `key`, `certificate`, `key-id`,
/// `authority`, `server-authority` (hyphenated, not underscored -- two
/// real provisioned devices' files under `~/provisioning` use underscores,
/// but those predate the parser and are themselves wrong).
///
/// `Debug` is safe to derive: `private_key` redacts itself.
#[derive(Debug, Clone)]
pub struct FactoryCredential {
    /// Rendered into the yaml as SEC1 PEM (`BEGIN EC PRIVATE KEY`), not
    /// PKCS#8 PEM, matching the known-good files -- so the conversion from
    /// what this holds is not a no-op.
    pub private_key: PrivateKey,
    /// The IDevID: CN is the device's serial, issued by the manufacturing
    /// account's factory authority.
    pub certificate_der: Vec<u8>,
    /// Anchor for verifying an RFC 8366 ownership voucher, should this
    /// device ever be transferred to another account.
    pub certificate_authority_der: Vec<u8>,
    /// Anchor for verifying the platform's signed response to the
    /// device's *first* bootstrap request -- only obtainable from the
    /// factory-device call itself.
    pub server_certificate_authority_der: Vec<u8>,
    /// The keyid the device puts on its own RFC 9421 request signatures
    /// later -- not derivable from the certificate itself, so it has to
    /// travel alongside it. `remora-factory.yaml`'s `key-id` (a required
    /// field there with no serde default -- get the spelling right).
    pub key_id: String,
}

/// Which serial a device is manufactured under -- exactly one of the two
/// shapes `POST /remora/v1/factory-device` accepts (`serialNumberPolicyName`
/// xor `deviceName`). `force` lives on the explicit arm only because the
/// platform refuses it alongside a policy: a policy always allocates a
/// fresh serial, so there is never an existing IDevID to replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceSerial {
    /// Let the platform allocate a new serial from this policy (e.g.
    /// `hubs`). The only way to be sure the serial has never been used.
    FromPolicy(String),
    /// A serial chosen outside the platform. Refused (409) if it was
    /// already manufactured, unless `force` -- which re-signs it and
    /// revokes the old IDevID (a mis-flashed board, a reused test unit).
    Explicit { device_name: String, force: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceSerialError {
    #[error("give either a device name or a serial number policy, not both")]
    Both,
    #[error("give a device name or a serial number policy")]
    Neither,
    #[error("force only applies to an explicit device name, not a serial number policy")]
    ForceWithPolicy,
}

impl DeviceSerial {
    /// Builds a `DeviceSerial` from the flat optional fields a CLI or a
    /// batch recipe carries, rejecting the combinations the platform would.
    pub fn from_parts(
        device_name: Option<String>,
        serial_number_policy: Option<String>,
        force: bool,
    ) -> Result<Self, DeviceSerialError> {
        match (device_name, serial_number_policy) {
            (Some(_), Some(_)) => Err(DeviceSerialError::Both),
            (None, None) => Err(DeviceSerialError::Neither),
            (None, Some(_)) if force => Err(DeviceSerialError::ForceWithPolicy),
            (None, Some(policy)) => Ok(Self::FromPolicy(policy)),
            (Some(device_name), None) => Ok(Self::Explicit { device_name, force }),
        }
    }
}

/// What a successful `provision` reports back to its caller: the serial
/// actually issued (to print on the label -- also the certificate's CN) and
/// the device's URN (the certificate's URI SAN).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedDevice {
    pub serial_number: String,
    pub factory_device_name: String,
}
