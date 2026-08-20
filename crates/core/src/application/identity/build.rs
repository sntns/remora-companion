use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    application::squashfs::{self, BuildSummary},
    model::squashfs::BuildOptions,
};

use super::error::Error;

/// Build `identity.squashfs` from `inputs` (arbitrary user-supplied files
/// and/or directories, same semantics as `application::squashfs::build`) plus
/// `hostname`, `machine-id`, and an ed25519 SSH host keypair — matching
/// `bootstrap-localdev.bb`'s `IDENTITY_DIR` convention (`hostname`,
/// `machine-id`, `ssh_host_ed25519_key`[`.pub`]).
///
/// `inputs` is not limited to those fields — a device's identity can carry
/// whatever additional files the caller supplies. `hostname`/`machine_id`
/// only cover two of the generated fields; the SSH host keypair is always
/// freshly generated (like `machine-id`, it isn't something a caller would
/// meaningfully override by value) unless `inputs` already supplies both
/// `ssh_host_ed25519_key` and `ssh_host_ed25519_key.pub`, in which case that
/// pair wins and nothing is generated. Same precedence rule for `hostname`/
/// `machine-id`: if `inputs` already provides a file at one of those
/// root-level paths, that file wins and nothing is generated for it.
pub fn build(
    inputs: &[PathBuf],
    hostname: Option<&str>,
    machine_id: Option<&str>,
    output: &Path,
) -> Result<BuildSummary, Error> {
    let scratch = scratch_dir();
    let mut wrote_any = false;

    if !already_provides(inputs, "hostname") {
        let value = hostname.unwrap_or("remora");
        write_scratch_file(&scratch, "hostname", format!("{value}\n").as_bytes())?;
        wrote_any = true;
    }
    if !already_provides(inputs, "machine-id") {
        let value = machine_id
            .map(str::to_string)
            .unwrap_or_else(generate_machine_id);
        write_scratch_file(&scratch, "machine-id", format!("{value}\n").as_bytes())?;
        wrote_any = true;
    }
    if !(already_provides(inputs, "ssh_host_ed25519_key")
        && already_provides(inputs, "ssh_host_ed25519_key.pub"))
    {
        let (private_key, public_key) = generate_ssh_host_keypair()?;
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
    let result = squashfs::build(&all_inputs, output, &options).map_err(Error::from);

    if wrote_any {
        let _ = fs::remove_dir_all(&scratch);
    }
    result
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

fn scratch_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "remora-etcher-identity-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    dir
}

fn write_scratch_file(scratch: &Path, name: &str, contents: &[u8]) -> Result<(), Error> {
    fs::create_dir_all(scratch).map_err(|source| Error::CreateScratchDir {
        path: scratch.to_path_buf(),
        source,
    })?;
    let path = scratch.join(name);
    fs::write(&path, contents).map_err(|source| Error::WriteScratchFile { path, source })
}

/// `dbus-uuidgen` format: 32 lowercase hex characters, no dashes.
fn generate_machine_id() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh ed25519 SSH host keypair, matching `ssh-keygen -t ed25519`'s own
/// output shape: `(private_key_openssh_pem, public_key_openssh_line)`.
fn generate_ssh_host_keypair() -> Result<(String, String), Error> {
    let private_key =
        ssh_key::PrivateKey::random(&mut ssh_key::rand_core::OsRng, ssh_key::Algorithm::Ed25519)?;
    let private_openssh = private_key.to_openssh(ssh_key::LineEnding::LF)?;
    let mut public_openssh = private_key.public_key().to_openssh()?;
    public_openssh.push('\n');
    Ok((private_openssh.to_string(), public_openssh))
}

/// `chmod 600` — SSH refuses to use a host key file that's group/world
/// readable, matching `bootstrap-localdev.bb`'s own `install -m 600`.
#[cfg(unix)]
fn restrict_to_owner_only(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        Error::WriteScratchFile {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_to_owner_only(_path: &Path) -> Result<(), Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "remora-etcher-identity-test-{}-{}",
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
        let summary = build(std::slice::from_ref(&extra), None, None, &output).unwrap();

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
        let summary = build(std::slice::from_ref(&dir), Some("ignored"), None, &output).unwrap();

        // hostname (user-supplied) + machine-id + ssh_host_ed25519_key(.pub) (generated).
        assert_eq!(summary.entry_count, 4);
    }

    #[test]
    fn generates_a_valid_ed25519_ssh_host_keypair() {
        let dir = tempdir();
        let output = dir.join("../identity3.squashfs");
        build(&[], None, None, &output).unwrap();

        // Dev-only real tool, mirroring how other tests in this crate
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

        let entries = squashfs::inspect(&output).unwrap();
        let private_entry = entries
            .iter()
            .find(|e| e.path == std::path::Path::new("/ssh_host_ed25519_key"))
            .unwrap();
        assert_eq!(private_entry.permissions, 0o600);
    }
}
