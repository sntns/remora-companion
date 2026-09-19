/// A reference to a certificate stored in the platform's KMS. `urn` is the
/// keyid the device puts on its own RFC 9421 request signatures later --
/// it is not derivable from the certificate itself, so it has to travel
/// alongside it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CertificateReference {
    pub id: String,
    pub urn: String,
    pub name: String,
}

/// Everything a manufactured device needs to self-enroll at first boot,
/// per `sntns-platform`'s device-bootstrap contract: what the platform's
/// factory-device call hands back, plus the private key generated locally
/// to request it (the platform never sees this key -- only the CSR built
/// from it crosses the wire).
#[derive(Debug, Clone)]
pub struct FactoryCredential {
    /// The manufactured device's resource name (a full URN, not the bare
    /// serial), as returned by the platform.
    pub factory_device_name: String,
    pub certificate_reference: CertificateReference,
    /// PKCS#8 DER. Generated locally; never sent anywhere.
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
}
