use super::error::Result;

/// DI seam for `remora-identity-application`: swap real randomness
/// (a `machine-id` and an ed25519 SSH host keypair) for a deterministic test
/// double without touching the "generate defaults unless already provided"
/// use case.
pub trait KeygenAdapter: Send + Sync {
    /// `dbus-uuidgen` format: 32 lowercase hex characters, no dashes.
    fn machine_id(&self) -> String;

    /// A fresh ed25519 SSH host keypair, matching `ssh-keygen -t ed25519`'s
    /// own output shape: `(private_key_openssh_pem, public_key_openssh_line)`.
    fn ssh_host_keypair(&self) -> Result<(String, String)>;
}

/// Injectable handle to whatever `KeygenAdapter` was wired at startup.
#[derive(Clone)]
pub struct KeygenAdapterService(busybody::Service<Box<dyn KeygenAdapter>>);

impl KeygenAdapterService {
    pub fn new<T: KeygenAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for KeygenAdapterService {
    type Target = busybody::Service<Box<dyn KeygenAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
