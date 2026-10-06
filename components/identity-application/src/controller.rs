use std::{
    fs,
    path::{Path, PathBuf},
};

use error_stack::{Report, ResultExt};
use remora_identity::{
    adapter::KeygenAdapterService,
    application::{Error, IdentityServiceInterface, Result},
};
use remora_image::{application::ImageService, model::PartitionRole};
use remora_scratch::ScratchDir;
use remora_squashfs::{
    application::{BuildSummary, SquashfsService},
    model::BuildOptions,
};

const SSH_HOST_KEY: &str = "ssh_host_ed25519_key";
const SSH_HOST_KEY_PUB: &str = "ssh_host_ed25519_key.pub";

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

#[async_trait::async_trait]
impl IdentityServiceInterface for IdentityControllerImpl {
    async fn build(
        &self,
        inputs: &[PathBuf],
        hostname: Option<&str>,
        machine_id: Option<&str>,
        output: &Path,
    ) -> Result<BuildSummary> {
        let keygen = self.keygen.clone();
        let (inputs_owned, hostname, machine_id) = (
            inputs.to_vec(),
            hostname.map(str::to_string),
            machine_id.map(str::to_string),
        );
        // Holds the generated files, the SSH host private key among them,
        // until it's dropped at the end of this function, however it ends.
        let generated = blocking(move || {
            generate_missing(
                &keygen,
                &inputs_owned,
                hostname.as_deref(),
                machine_id.as_deref(),
            )
        })
        .await?;

        let mut all_inputs = inputs.to_vec();
        all_inputs.extend(generated.as_ref().map(|s| s.path().to_path_buf()));

        let options = BuildOptions {
            root_mode: 0o755,
            ..Default::default()
        };
        self.squashfs
            .build(
                &all_inputs,
                output,
                &options,
                &remora_progress::OperationContext::noop(),
            )
            .await
            .change_context(Error::Squashfs)
    }

    async fn create(
        &self,
        inputs: &[PathBuf],
        hostname: Option<&str>,
        machine_id: Option<&str>,
        image: &Path,
    ) -> Result<u64> {
        let scratch =
            ScratchDir::new("remora-identity-create").change_context(Error::CreateScratchDir)?;
        let built = scratch.join("identity.squashfs");
        self.build(inputs, hostname, machine_id, &built).await?;

        let contents = blocking(move || {
            fs::read(&built).change_context_lazy(|| Error::ReadBuiltImage(built.clone()))
        })
        .await?;
        drop(scratch);

        self.image
            .ensure_dir_by_role(image, PartitionRole::Shared, "/remora", 0o755)
            .await
            .change_context(Error::Image)?;
        self.image
            .inject_by_role(
                image,
                PartitionRole::Shared,
                "/remora/identity",
                &contents,
                0o644,
            )
            .await
            .change_context(Error::Image)?;

        Ok(contents.len() as u64)
    }
}

/// Run `op` on the blocking pool, off the runtime threads.
async fn blocking<T: Send + 'static>(op: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(op)
        .await
        .expect("identity worker panicked")
}

/// A scratch directory holding whichever identity files `inputs` doesn't
/// supply, or `None` if it supplies them all.
fn generate_missing(
    keygen: &KeygenAdapterService,
    inputs: &[PathBuf],
    hostname: Option<&str>,
    machine_id: Option<&str>,
) -> Result<Option<ScratchDir>> {
    let has_key = already_provides(inputs, SSH_HOST_KEY);
    let has_key_pub = already_provides(inputs, SSH_HOST_KEY_PUB);
    // A generated pair would put a second file of the supplied half's name
    // in the squashfs root, and wouldn't match it anyway: a lone half is
    // refused rather than guessed around.
    if has_key != has_key_pub {
        let (present, missing) = if has_key {
            (SSH_HOST_KEY, SSH_HOST_KEY_PUB)
        } else {
            (SSH_HOST_KEY_PUB, SSH_HOST_KEY)
        };
        return Err(Report::new(Error::HalfSshHostKeypair { present, missing }));
    }
    let has_hostname = already_provides(inputs, "hostname");
    let has_machine_id = already_provides(inputs, "machine-id");
    if has_key && has_hostname && has_machine_id {
        return Ok(None);
    }

    let scratch = ScratchDir::new("remora-identity").change_context(Error::CreateScratchDir)?;
    if !has_hostname {
        let value = hostname.unwrap_or("remora");
        write_scratch_file(&scratch, "hostname", format!("{value}\n").as_bytes())?;
    }
    if !has_machine_id {
        let value = machine_id
            .map(str::to_string)
            .unwrap_or_else(|| keygen.machine_id());
        write_scratch_file(&scratch, "machine-id", format!("{value}\n").as_bytes())?;
    }
    if !has_key {
        let (private_key, public_key) = keygen.ssh_host_keypair().change_context(Error::Keygen)?;
        // 0600 from creation on, never briefly readable under the umask's
        // mode -- and SSH refuses a group/world-readable host key anyway,
        // matching `bootstrap-localdev.bb`'s own `install -m 600`.
        scratch
            .write_private(SSH_HOST_KEY, private_key.as_bytes())
            .change_context_lazy(|| Error::WriteScratchFile(scratch.join(SSH_HOST_KEY)))?;
        write_scratch_file(&scratch, SSH_HOST_KEY_PUB, public_key.as_bytes())?;
    }
    Ok(Some(scratch))
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

fn write_scratch_file(scratch: &ScratchDir, name: &str, contents: &[u8]) -> Result<()> {
    let path = scratch.join(name);
    fs::write(&path, contents).change_context_lazy(|| Error::WriteScratchFile(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;

    fn controller() -> IdentityControllerImpl {
        let squashfs =
            SquashfsService::new(remora_squashfs_application::SquashfsControllerImpl::new(
                remora_fs_walk::FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
                remora_squashfs::adapter::SquashfsAdapterService::new(
                    remora_squashfs_adapter_backhand::SquashfsAdapterImpl,
                ),
            ));
        let image = ImageService::new(remora_image_application::ImageControllerImpl::new(
            remora_image::adapter::partition_table::PartitionTableAdapterService::new(
                remora_image_adapter_partition_table::PartitionTableAdapterImpl,
            ),
            Arc::new(remora_image_adapter_ext4::Ext4AdapterImpl),
            Arc::new(remora_image_adapter_vfat::VfatAdapterImpl),
            remora_fs_walk::FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
            remora_image::adapter::ext4::Ext4AdapterService::new(
                remora_image_adapter_ext4::Ext4AdapterImpl,
            ),
        ));
        IdentityControllerImpl::new(
            KeygenAdapterService::new(remora_identity_adapter_keygen::KeygenAdapterImpl),
            squashfs,
            image,
        )
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "remora-identity-application-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn generates_hostname_machine_id_and_ssh_host_key_when_absent() {
        let extra = tempdir();
        fs::File::create(extra.join("activation"))
            .unwrap()
            .write_all(b"some-token\n")
            .unwrap();

        let output = extra.join("../identity.squashfs");
        let summary = controller()
            .build(std::slice::from_ref(&extra), None, None, &output)
            .await
            .unwrap();

        // activation, hostname, machine-id, ssh_host_ed25519_key(.pub).
        assert_eq!(summary.entry_count, 5);
    }

    #[tokio::test]
    async fn respects_a_user_supplied_hostname_file() {
        let dir = tempdir();
        fs::File::create(dir.join("hostname"))
            .unwrap()
            .write_all(b"my-custom-host\n")
            .unwrap();

        let output = dir.join("../identity2.squashfs");
        let summary = controller()
            .build(std::slice::from_ref(&dir), Some("ignored"), None, &output)
            .await
            .unwrap();

        // hostname (user-supplied) + machine-id + ssh_host_ed25519_key(.pub) (generated).
        assert_eq!(summary.entry_count, 4);
    }

    #[tokio::test]
    async fn refuses_half_an_ssh_host_keypair() {
        for half in [SSH_HOST_KEY, SSH_HOST_KEY_PUB] {
            let dir = tempdir();
            fs::write(dir.join(half), b"supplied\n").unwrap();

            let output = dir.join("../identity-half.squashfs");
            let err = controller()
                .build(std::slice::from_ref(&dir), None, None, &output)
                .await
                .unwrap_err();
            assert!(
                matches!(
                    err.current_context(),
                    Error::HalfSshHostKeypair { present, .. } if *present == half
                ),
                "{err:?}"
            );
        }
    }

    #[tokio::test]
    async fn keeps_a_whole_supplied_ssh_host_keypair() {
        let dir = tempdir();
        fs::write(dir.join(SSH_HOST_KEY), b"private\n").unwrap();
        fs::write(dir.join(SSH_HOST_KEY_PUB), b"public\n").unwrap();

        let output = dir.join("../identity-pair.squashfs");
        let summary = controller()
            .build(std::slice::from_ref(&dir), None, None, &output)
            .await
            .unwrap();

        // The supplied pair + generated hostname and machine-id.
        assert_eq!(summary.entry_count, 4);
    }

    #[tokio::test]
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "requires unsquashfs, a Linux-only dev tool"
    )]
    async fn generates_a_valid_ed25519_ssh_host_keypair() {
        let dir = tempdir();
        let output = dir.join("../identity3.squashfs");
        controller().build(&[], None, None, &output).await.unwrap();

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

        let entries = controller().squashfs.inspect(&output).await.unwrap();
        let private_entry = entries
            .iter()
            .find(|e| e.path == std::path::Path::new("/ssh_host_ed25519_key"))
            .unwrap();
        assert_eq!(private_entry.permissions, 0o600);
    }
}
