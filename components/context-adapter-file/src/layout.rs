use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// The configuration directory: `$RMRA_CONFIG` when set, otherwise
/// `$XDG_CONFIG_HOME/rmra` (or `~/.config/rmra`) on Linux and macOS --
/// a CLI's config, not an app's, so not `~/Library/Application Support` --
/// and `%APPDATA%\rmra` on Windows.
pub fn default_root() -> PathBuf {
    if let Some(root) = std::env::var_os("RMRA_CONFIG").filter(|v| !v.is_empty()) {
        return PathBuf::from(root);
    }
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        // std's home_dir is sound again since Rust 1.87 ($HOME first).
        .or_else(|| std::env::home_dir().map(|home| home.join(".config")));
    base.unwrap_or_else(|| PathBuf::from(".")).join("rmra")
}

/// ```text
/// <root>/config.json          {"version": 2, "currentContext": "eu2", "contexts": {…}}
/// <root>/credentials.json     0600: each login's credentials, by context name
/// <root>/.lock                held while a change is being made
/// ```
///
/// What the previous layout left, until migrated (see `migrate`):
///
/// ```text
/// <root>/config.json                         {"currentContext": "eu2"}
/// <root>/contexts/<name>/meta.json           the context (no secrets)
/// <root>/contexts/<name>/credentials.json    0600
/// ```
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pub root: PathBuf,
}

impl Layout {
    pub fn config(&self) -> PathBuf {
        self.root.join("config.json")
    }

    pub fn credentials(&self) -> PathBuf {
        self.root.join("credentials.json")
    }

    pub fn lock(&self) -> PathBuf {
        self.root.join(".lock")
    }

    /// The previous layout's directory of contexts.
    pub fn legacy_contexts(&self) -> PathBuf {
        self.root.join("contexts")
    }

    /// Where a migration keeps the previous layout: `backup-v1`, or the
    /// first `backup-v1-<n>` free.
    pub fn backup(&self) -> PathBuf {
        let first = self.root.join("backup-v1");
        if !first.exists() {
            return first;
        }
        (2..)
            .map(|n| self.root.join(format!("backup-v1-{n}")))
            .find(|dir| !dir.exists())
            .expect("some backup name is free")
    }
}

/// Reads a file, `None` when it doesn't exist.
pub(crate) fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Writes `bytes` to `path` atomically (a sibling temp file renamed over
/// it), creating private (0700) parent directories as needed. `secret`
/// files are created 0600 before any byte is written.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], secret: bool) -> io::Result<()> {
    let parent = path.parent().expect("store paths always have a parent");
    create_private_dir(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if secret { 0o600 } else { 0o644 });
        }
        #[cfg(not(unix))]
        let _ = secret;
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

pub(crate) fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}
