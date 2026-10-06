use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt};
use remora_update::{
    adapter::installer::{Error, InstallerAdapter, Result},
    model::{App, Installation, Release},
};
use serde::Deserialize;

/// Installs releases with the installers dist publishes beside them
/// (`<app>-installer.sh`, `<app>-installer.ps1`), forced into the directory
/// the running binary lives in -- the same thing as running the documented
/// `curl … | sh` again, minus touching PATH.
///
/// Knows an installer-made installation by dist's install receipt
/// (`<config>/<app>/<app>-receipt.json`), which records where it installed.
pub struct DistInstallerImpl {
    client: reqwest::Client,
    downloads: String,
    receipts: Option<PathBuf>,
}

impl DistInstallerImpl {
    /// `repo` is `owner/name` of the releases repo.
    pub fn new(repo: &str) -> Self {
        Self {
            client: super::feed::client(),
            downloads: format!("https://github.com/{repo}/releases/download"),
            receipts: None,
        }
    }

    /// Against another download root and receipt directory, e.g. in tests.
    pub fn with_locations(downloads: &str, receipts: PathBuf) -> Self {
        Self {
            client: super::feed::client(),
            downloads: downloads.trim_end_matches('/').to_owned(),
            receipts: Some(receipts),
        }
    }

    /// Where dist's installers write the receipt for `app`, as they do it:
    /// `$XDG_CONFIG_HOME/<app>` or `~/.config/<app>`, `%LOCALAPPDATA%\<app>`
    /// on Windows.
    fn receipt_path(&self, app: &str) -> Option<PathBuf> {
        let dir = match &self.receipts {
            Some(dir) => dir.clone(),
            None if cfg!(windows) => PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join(app),
            None => std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::home_dir().map(|home| home.join(".config")))?
                .join(app),
        };
        Some(dir.join(format!("{app}-receipt.json")))
    }
}

#[derive(Deserialize)]
struct Receipt {
    install_prefix: PathBuf,
}

/// The directory a binary is really in, symlinks resolved.
fn real_dir(executable: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    resolved.parent().map(Path::to_path_buf).unwrap_or_default()
}

#[async_trait::async_trait]
impl InstallerAdapter for DistInstallerImpl {
    async fn installation(&self, app: &App) -> Result<Installation> {
        let dir = real_dir(&app.executable);
        // Homebrew keeps every formula under …/Cellar/<formula>/<version>/.
        if dir.components().any(|c| c.as_os_str() == "Cellar") {
            return Ok(Installation::Homebrew {
                formula: format!("sntns/tap/{}", app.name),
            });
        }
        if let Some(path) = self.receipt_path(&app.name) {
            if let Ok(bytes) = tokio::fs::read(&path).await {
                let receipt: Receipt = serde_json::from_slice(&bytes)
                    .change_context(Error::Inspect)
                    .attach_with(|| path.display().to_string())?;
                // A flat install records the bin dir itself; a hierarchical
                // one, its parent.
                let prefix = std::fs::canonicalize(&receipt.install_prefix)
                    .unwrap_or(receipt.install_prefix);
                if prefix == dir || prefix.join("bin") == dir {
                    return Ok(Installation::Installer { dir });
                }
            }
        }
        Ok(Installation::Unmanaged { dir })
    }

    async fn install(&self, app: &App, release: &Release, dir: &Path) -> Result<()> {
        let (script, program, args): (_, _, &[&str]) = if cfg!(windows) {
            (
                format!("{}-installer.ps1", app.name),
                "powershell",
                &["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"],
            )
        } else {
            (format!("{}-installer.sh", app.name), "sh", &[])
        };
        let url = format!("{}/{}/{script}", self.downloads, release.tag);
        let body = self
            .client
            .get(&url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .change_context(Error::Download)
            .attach_with(|| url.clone())?
            .bytes()
            .await
            .change_context(Error::Download)?;
        let workdir = tempfile::tempdir().change_context(Error::Download)?;
        let installer = workdir.path().join(&script);
        tokio::fs::write(&installer, &body)
            .await
            .change_context(Error::Download)?;

        // Windows can't overwrite a running executable, but can rename it:
        // move it aside so the installer can write the new one in its place.
        #[cfg(windows)]
        let aside = {
            let aside = app.executable.with_extension("exe.old");
            let _ = std::fs::remove_file(&aside);
            std::fs::rename(&app.executable, &aside).change_context(Error::Install)?;
            aside
        };

        let output = tokio::process::Command::new(program)
            .args(args)
            .arg(&installer)
            .env("CARGO_DIST_FORCE_INSTALL_DIR", dir)
            .env("INSTALLER_NO_MODIFY_PATH", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .change_context(Error::Install)
            .attach_with(|| format!("running {program} {}", installer.display()))?;
        if output.status.success() {
            return Ok(());
        }

        #[cfg(windows)]
        if !app.executable.exists() {
            let _ = std::fs::rename(&aside, &app.executable);
        }
        // What the installer said is the useful part of its failure.
        let said = String::from_utf8_lossy(&output.stderr);
        let said = if said.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout).into_owned()
        } else {
            said.into_owned()
        };
        let tail: Vec<_> = said
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Err(Report::new(Error::Install)
            .attach(format!("{} exited with {}", script, output.status))
            .attach(tail.join("\n")))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use remora_update::{
        adapter::feed::{self, ReleaseFeedAdapter},
        model::Version,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::GithubReleaseFeedImpl;

    /// A tiny HTTP server: the GitHub API's latest release, and a release
    /// download whose "installer" writes a new rmra where it's told to.
    async fn serve(latest: Option<&'static str>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 4096];
                let read = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                let (status, body) = match (path.as_str(), latest) {
                    ("/repos/sntns/releases/releases/latest", Some(tag)) => {
                        ("200 OK", format!(r#"{{"tag_name":"{tag}","draft":false}}"#))
                    }
                    ("/repos/sntns/releases/releases/latest", None) => ("404 Not Found", "{}".into()),
                    ("/download/v0.4.0/rmra-installer.sh", _) => (
                        "200 OK",
                        "#!/bin/sh\nset -e\n[ \"$INSTALLER_NO_MODIFY_PATH\" = 1 ]\n\
                         printf 'new rmra 0.4.0' > \"$CARGO_DIST_FORCE_INSTALL_DIR/rmra\"\n"
                            .into(),
                    ),
                    ("/download/v0.5.0/rmra-installer.sh", _) => (
                        "200 OK",
                        "#!/bin/sh\necho 'ERROR: unable to download rmra-x86_64-unknown-linux-musl.tar.xz' >&2\nexit 1\n"
                            .into(),
                    ),
                    _ => ("404 Not Found", String::new()),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        address
    }

    fn app(executable: PathBuf) -> App {
        App {
            name: "rmra".into(),
            version: Version::parse("0.3.0").unwrap(),
            executable,
        }
    }

    #[tokio::test]
    async fn reads_the_latest_release() {
        let address = serve(Some("v0.4.0")).await;
        let latest = GithubReleaseFeedImpl::with_api(&address, "sntns/releases")
            .latest()
            .await
            .unwrap();
        assert_eq!(
            (latest.tag.as_str(), latest.version.to_string().as_str()),
            ("v0.4.0", "0.4.0")
        );

        let address = serve(None).await;
        let report = GithubReleaseFeedImpl::with_api(&address, "sntns/releases")
            .latest()
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), feed::Error::NoRelease));
    }

    #[tokio::test]
    async fn knows_how_the_binary_was_installed() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("rmra"), "old").unwrap();
        let receipts = root.path().join("config");
        std::fs::create_dir_all(&receipts).unwrap();
        let installer = DistInstallerImpl::with_locations("http://unused", receipts.clone());

        assert!(matches!(
            installer
                .installation(&app(bin.join("rmra")))
                .await
                .unwrap(),
            Installation::Unmanaged { .. }
        ));
        std::fs::write(
            receipts.join("rmra-receipt.json"),
            format!(
                r#"{{"install_prefix":"{}","binaries":["rmra"]}}"#,
                bin.display()
            ),
        )
        .unwrap();
        assert!(matches!(
            installer
                .installation(&app(bin.join("rmra")))
                .await
                .unwrap(),
            Installation::Installer { .. }
        ));

        let cellar = root.path().join("homebrew/Cellar/rmra/0.3.0/bin");
        std::fs::create_dir_all(&cellar).unwrap();
        std::fs::write(cellar.join("rmra"), "brewed").unwrap();
        assert_eq!(
            installer
                .installation(&app(cellar.join("rmra")))
                .await
                .unwrap(),
            Installation::Homebrew {
                formula: "sntns/tap/rmra".into()
            }
        );
    }

    #[tokio::test]
    async fn runs_the_release_installer_into_the_binarys_directory() {
        let address = serve(Some("v0.4.0")).await;
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("rmra"), "old").unwrap();
        let installer = DistInstallerImpl::with_locations(
            &format!("{address}/download"),
            root.path().join("cfg"),
        );
        let release = |tag: &str| Release {
            version: Version::parse(tag).unwrap(),
            tag: tag.into(),
        };

        installer
            .install(
                &app(root.path().join("rmra")),
                &release("v0.4.0"),
                root.path(),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("rmra")).unwrap(),
            "new rmra 0.4.0"
        );

        let report = installer
            .install(
                &app(root.path().join("rmra")),
                &release("v0.5.0"),
                root.path(),
            )
            .await
            .unwrap_err();
        assert!(
            format!("{report:?}").contains("unable to download"),
            "{report:?}"
        );
    }
}
