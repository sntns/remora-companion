use std::{
    fs,
    path::{Path, PathBuf},
};

use error_stack::{Report, ResultExt};
use remora_etcher_identity::{
    adapter::KeygenAdapterService,
    application::{Error, IdentityServiceInterface, Result},
};
use remora_etcher_image::{application::ImageService, model::PartitionRole};
use remora_etcher_squashfs::{
    application::{BuildSummary, SquashfsService},
    model::BuildOptions,
};

/// The identity vertical's use case: fills in whichever of
/// hostname/machine-id/SSH-host-keypair `inputs` doesn't already supply,
/// via the injected `KeygenAdapterService`, then hands the merged input list
/// to the squashfs vertical (`SquashfsService`) to actually build — and, for
/// `create`, injects the result into the image's shared partition via the
/// image vertical (`ImageService`).
pub struct IdentityControllerImpl {
    keygen: KeygenAdapterService,
    squashfs: SquashfsService,
    image: ImageService,
}

impl IdentityControllerImpl {
    pub fn new(
        keygen: KeygenAdapterService,
        squashfs: SquashfsService,
        image: ImageService,
    ) -> Self {
        Self {
            keygen,
            squashfs,
            image,
        }
    }
}

impl IdentityServiceInterface for IdentityControllerImpl {
    fn build(
        &self,
        inputs: &[PathBuf],
        hostname: Option<&str>,
        machine_id: Option<&str>,
        output: &Path,
    ) -> Result<BuildSummary> {
        let scratch = remora_etcher_scratch::unique_path("remora-etcher-identity");
        let mut wrote_any = false;

        if !already_provides(inputs, "hostname") {
            let value = hostname.unwrap_or("remora");
            write_scratch_file(&scratch, "hostname", format!("{value}\n").as_bytes())?;
            wrote_any = true;
        }
        if !already_provides(inputs, "machine-id") {
            let value = machine_id
                .map(str::to_string)
                .unwrap_or_else(|| self.keygen.machine_id());
            write_scratch_file(&scratch, "machine-id", format!("{value}\n").as_bytes())?;
            wrote_any = true;
        }
        if !(already_provides(inputs, "ssh_host_ed25519_key")
            && already_provides(inputs, "ssh_host_ed25519_key.pub"))
        {
            let (private_key, public_key) = self
                .keygen
                .ssh_host_keypair()
                .change_context(Error::Keygen)?;
            write_scratch_file(&scratch, "ssh_host_ed25519_key", private_key.as_bytes())?;
            restrict_to_owner_only(&scratch.join("ssh_host_ed25519_key"))?;
            write_scratch_file(&scratch, "ssh_host_ed25519_key.pub", public_key.as_bytes())?;
            wrote_any = true;
        }

        let mut all_inputs = inputs.to_vec();
        if wrote_any {
            all_inputs.push(scratch.clone());
        }

        let options = BuildOptions {
            root_mode: 0o755,
            ..Default::default()
        };
        let result = self
            .squashfs
            .build(&all_inputs, output, &options)
            .change_context(Error::Squashfs);

        if wrote_any {
            let _ = fs::remove_dir_all(&scratch);
        }
        result
    }

    fn create(
        &self,
        inputs: &[PathBuf],
        hostname: Option<&str>,
        machine_id: Option<&str>,
        image: &Path,
    ) -> Result<u64> {
        let temp = remora_etcher_scratch::unique_path("remora-etcher-identity-create");
        self.build(inputs, hostname, machine_id, &temp)?;

        let contents =
            fs::read(&temp).map_err(|_| Report::new(Error::ReadBuiltImage(temp.clone())));
        let _ = fs::remove_file(&temp);
        let contents = contents?;

        self.image
            .ensure_dir_by_role(image, PartitionRole::Shared, "/remora", 0o755)
            .change_context(Error::Image)?;
        self.image
            .inject_by_role(
                image,
                PartitionRole::Shared,
                "/remora/identity",
                &contents,
                0o644,
            )
            .change_context(Error::Image)?;

        Ok(contents.len() as u64)
    }
}

/// Whether one of `inputs` already supplies a root-level file named
/// `file_name` — a directory input contributing it directly, or a file
/// input that *is* it.
fn already_provides(inputs: &[PathBuf], file_name: &str) -> bool {
    inputs.iter().any(|input| {
        if input.is_dir() {
            input.join(file_name).is_file()
        } else {
            input.file_name().is_some_and(|n| n == file_name)
        }
    })
}

fn write_scratch_file(scratch: &Path, name: &str, contents: &[u8]) -> Result<()> {
    fs::create_dir_all(scratch)
        .map_err(|_| Report::new(Error::CreateScratchDir(scratch.to_path_buf())))?;
    let path = scratch.join(name);
    fs::write(&path, contents).map_err(|_| Report::new(Error::WriteScratchFile(path)))
}

/// `chmod 600` — SSH refuses to use a host key file that's group/world
/// readable, matching `bootstrap-localdev.bb`'s own `install -m 600`.
#[cfg(unix)]
fn restrict_to_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| Report::new(Error::WriteScratchFile(path.to_path_buf())))
}

#[cfg(not(unix))]
fn restrict_to_owner_only(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;

    fn controller() -> IdentityControllerImpl {
        let squashfs = SquashfsService::new(
            remora_etcher_squashfs_application::SquashfsControllerImpl::new(
                remora_etcher_fs_walk::FsWalkAdapterService::new(
                    remora_etcher_fs_walk::FsWalkAdapterImpl,
                ),
                remora_etcher_squashfs::adapter::SquashfsAdapterService::new(
                    remora_etcher_squashfs_adapter_backhand::SquashfsAdapterImpl,
                ),
            ),
        );
        let image = ImageService::new(remora_etcher_image_application::ImageControllerImpl::new(
            remora_etcher_image::adapter::partition_table::PartitionTableAdapterService::new(
                remora_etcher_image_adapter_partition_table::PartitionTableAdapterImpl,
            ),
            Arc::new(remora_etcher_image_adapter_ext4::Ext4AdapterImpl),
            Arc::new(remora_etcher_image_adapter_vfat::VfatAdapterImpl),
            remora_etcher_fs_walk::FsWalkAdapterService::new(
                remora_etcher_fs_walk::FsWalkAdapterImpl,
            ),
        ));
        IdentityControllerImpl::new(
            KeygenAdapterService::new(remora_etcher_identity_adapter_keygen::KeygenAdapterImpl),
            squashfs,
            image,
        )
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "remora-etcher-identity-application-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn generates_hostname_machine_id_and_ssh_host_key_when_absent() {
        let extra = tempdir();
        fs::File::create(extra.join("activation"))
            .unwrap()
            .write_all(b"some-token\n")
            .unwrap();

        let output = extra.join("../identity.squashfs");
        let summary = controller()
            .build(std::slice::from_ref(&extra), None, None, &output)
            .unwrap();

        // activation, hostname, machine-id, ssh_host_ed25519_key(.pub).
        assert_eq!(summary.entry_count, 5);
    }

    #[test]
    fn respects_a_user_supplied_hostname_file() {
        let dir = tempdir();
        fs::File::create(dir.join("hostname"))
            .unwrap()
            .write_all(b"my-custom-host\n")
            .unwrap();

        let output = dir.join("../identity2.squashfs");
        let summary = controller()
            .build(std::slice::from_ref(&dir), Some("ignored"), None, &output)
            .unwrap();

        // hostname (user-supplied) + machine-id + ssh_host_ed25519_key(.pub) (generated).
        assert_eq!(summary.entry_count, 4);
    }

    #[test]
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "requires unsquashfs, a Linux-only dev tool"
    )]
    fn generates_a_valid_ed25519_ssh_host_keypair() {
        let dir = tempdir();
        let output = dir.join("../identity3.squashfs");
        controller().build(&[], None, None, &output).unwrap();

        // Dev-only real tool, mirroring how other tests in this workspace
        // validate squashfs output — never shelled out to by the shipped
        // binary.
        let private_pem = String::from_utf8(
            std::process::Command::new("unsquashfs")
                .args(["-cat", output.to_str().unwrap(), "ssh_host_ed25519_key"])
                .output()
                .expect("unsquashfs not available")
                .stdout,
        )
        .unwrap();
        let private_key = ssh_key::PrivateKey::from_openssh(&private_pem).unwrap();
        assert_eq!(private_key.algorithm(), ssh_key::Algorithm::Ed25519);

        let public_line = String::from_utf8(
            std::process::Command::new("unsquashfs")
                .args(["-cat", output.to_str().unwrap(), "ssh_host_ed25519_key.pub"])
                .output()
                .expect("unsquashfs not available")
                .stdout,
        )
        .unwrap();
        let public_key = ssh_key::PublicKey::from_openssh(public_line.trim()).unwrap();
        assert_eq!(public_key, *private_key.public_key());

        let entries = controller().squashfs.inspect(&output).unwrap();
        let private_entry = entries
            .iter()
            .find(|e| e.path == std::path::Path::new("/ssh_host_ed25519_key"))
            .unwrap();
        assert_eq!(private_entry.permissions, 0o600);
    }
}
