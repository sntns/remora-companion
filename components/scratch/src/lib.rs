use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// A private scratch directory under the OS temp dir, removed with
/// everything in it when dropped -- so on every way out of the code using
/// it, early `?` returns included.
///
/// The shared temp dir is writable by every local user, and scratch files
/// can be secrets (identity's SSH host private key), so the directory is
/// created exclusively, never reused: a path someone created first (a
/// directory to read from, a symlink to redirect writes) is an error, and
/// another name is tried. On unix it's mode 0700 from creation on.
///
/// Plain type rather than a DI-injected port: which directory scratch
/// files live in isn't a business decision any test needs to substitute —
/// tests that touch this already want a real temp dir, same as production.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// A fresh scratch directory named `<prefix>-<random>`.
    pub fn new(prefix: &str) -> io::Result<Self> {
        const ATTEMPTS: u32 = 16;
        let mut last_err = None;
        for _ in 0..ATTEMPTS {
            let path = std::env::temp_dir().join(format!("{prefix}-{:016x}", random()));
            match create_private_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last_err = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last_err.expect("at least one attempt was made"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.path.join(name)
    }

    /// Write `contents` to a new file `name` readable by its owner only
    /// (0600 on unix, from creation on: never briefly world-readable under
    /// the umask's mode). An existing file is an error, not overwritten.
    pub fn write_private(&self, name: impl AsRef<Path>, contents: &[u8]) -> io::Result<PathBuf> {
        use io::Write;
        let path = self.join(name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(&path)?.write_all(contents)?;
        Ok(path)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    // Not recursive: `create` fails on an existing path, symlink included.
    builder.create(path)
}

/// 64 unpredictable bits from std alone: `RandomState` is seeded from the
/// OS's randomness. Unpredictability only spares a retry; exclusive
/// creation is what keeps the directory private.
fn random() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    hasher.write_u32(std::process::id());
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_private_and_removed_on_drop() {
        let scratch = ScratchDir::new("remora-scratch-test").unwrap();
        let path = scratch.path().to_path_buf();
        let file = scratch.write_private("key", b"secret").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"secret");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o700);
            assert_eq!(mode(&file), 0o600);
        }

        drop(scratch);
        assert!(!path.exists());
    }

    #[test]
    fn write_private_refuses_an_existing_file() {
        let scratch = ScratchDir::new("remora-scratch-test").unwrap();
        scratch.write_private("key", b"first").unwrap();
        let err = scratch.write_private("key", b"second").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn never_reuses_an_existing_directory() {
        let existing = ScratchDir::new("remora-scratch-test").unwrap();
        let err = create_private_dir(existing.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_a_planted_symlink() {
        let target = ScratchDir::new("remora-scratch-test").unwrap();
        let link = std::env::temp_dir().join(format!("remora-scratch-link-{:016x}", random()));
        std::os::unix::fs::symlink(target.path(), &link).unwrap();
        let err = create_private_dir(&link).unwrap_err();
        let _ = fs::remove_file(&link);
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }
}
