/// Everything a manufactured device needs to self-enroll at first boot,
/// per `sntns-platform`'s device-bootstrap contract: what the platform's
/// factory-device call hands back, plus the private key generated locally
/// to request it (the platform never sees this key -- only the CSR built
/// from it crosses the wire).
///
/// Rendered as `remora-factory.yaml` at the application layer (PEM
/// encoding/SEC1 conversion are format-conversion concerns, not domain
/// data -- see `remora-etcher-factory-application`), matching the schema
/// read from two real provisioned devices' factory.yaml files: `url`,
/// `key`, `certificate`, `key_id`, `authority`, `server_authority`.
#[derive(Debug, Clone)]
pub struct FactoryCredential {
    /// PKCS#8 DER. Generated locally; never sent anywhere. Rendered into
    /// the yaml as SEC1 PEM (`BEGIN EC PRIVATE KEY`), not PKCS#8 PEM,
    /// matching the known-good files -- rcgen's own DER output is PKCS#8,
    /// so this conversion is not a no-op.
    pub private_key_der: Vec<u8>,
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
    /// travel alongside it. `remora-factory.yaml`'s `key_id`.
    pub key_id: String,
}
