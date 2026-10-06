mod version;

use std::path::PathBuf;

pub use version::Version;

/// A published release of the binary being updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The git tag it was published under (`v0.4.0`).
    pub tag: String,
}

/// How the running binary got where it is, which decides who updates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installation {
    /// By the release's own installer (shell/powershell), into `dir`: an
    /// update re-runs the installer there.
    Installer { dir: PathBuf },
    /// By Homebrew: `brew upgrade` owns updates, not the binary.
    Homebrew { formula: String },
    /// Some other way (built from source, copied by hand, a .deb...).
    Unmanaged { dir: PathBuf },
}

/// The binary asking to be updated.
#[derive(Debug, Clone)]
pub struct App {
    /// Its name, as releases and installers name it (`rmra`).
    pub name: String,
    pub version: Version,
    /// The running executable.
    pub executable: PathBuf,
}

/// What a check found.
#[derive(Debug, Clone)]
pub struct UpdateCheck {
    pub current: Version,
    pub latest: Release,
    pub installation: Installation,
}

impl UpdateCheck {
    pub fn available(&self) -> bool {
        self.latest.version > self.current
    }
}
