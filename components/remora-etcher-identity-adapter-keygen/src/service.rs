use error_stack::Report;
use remora_etcher_identity::adapter::{Error, KeygenAdapter, Result};

#[derive(Debug, Default, Clone, Copy)]
pub struct KeygenAdapterImpl;

impl KeygenAdapter for KeygenAdapterImpl {
    fn machine_id(&self) -> String {
        let bytes: [u8; 16] = rand::random();
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn ssh_host_keypair(&self) -> Result<(String, String)> {
        (|| -> std::result::Result<(String, String), ssh_key::Error> {
            let private_key = ssh_key::PrivateKey::random(
                &mut ssh_key::rand_core::OsRng,
                ssh_key::Algorithm::Ed25519,
            )?;
            let private_openssh = private_key.to_openssh(ssh_key::LineEnding::LF)?;
            let mut public_openssh = private_key.public_key().to_openssh()?;
            public_openssh.push('\n');
            Ok((private_openssh.to_string(), public_openssh))
        })()
        .map_err(|e| Report::new(Error::GenerateSshHostKey(e)))
    }
}
