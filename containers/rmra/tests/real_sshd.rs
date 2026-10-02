//! `rmra ssh` end to end against a real OpenSSH server: a fake gateway
//! certifies the throwaway key with a real user CA (`ssh-keygen -s`) and
//! carries the channel to a real `sshd` whose host key a real host CA
//! signed. Exercises everything the platform's contract promises an
//! operator: the ProxyCommand being rmra itself, host pinning through
//! `@cert-authority` + HostKeyAlias, the certificate's principal, and the
//! relay's half-close and hangup handling as ssh ends its ProxyCommand.
//!
//! Linux only (a user-mode sshd is dependable there, including on CI
//! runners), and skipped (passes trivially) where `/usr/sbin/sshd` or `ssh`
//! is missing.
#![cfg(target_os = "linux")]

use std::{
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
};

use remora_platform_grpc::sntns::service::{remorachannel::v1 as pb, v1::ResourceReference};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

const SERIAL: &str = "525400C0FFEE";
const ALIAS: &str = "525400c0ffee";

fn keygen(args: &[&str]) {
    let status = std::process::Command::new("ssh-keygen")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "ssh-keygen {args:?}");
}

struct Gateway {
    dir: PathBuf,
    sshd_port: u16,
    login: String,
}

type Responses =
    Pin<Box<dyn Stream<Item = Result<pb::ChannelServiceOpenDeviceChannelResponse, Status>> + Send>>;

fn message(
    response: pb::channel_service_open_device_channel_response::Response,
) -> Result<pb::ChannelServiceOpenDeviceChannelResponse, Status> {
    Ok(pb::ChannelServiceOpenDeviceChannelResponse {
        response: Some(response),
    })
}

#[tonic::async_trait]
impl pb::channel_service_server::ChannelService for Gateway {
    type OpenDeviceChannelStream = Responses;

    /// Bridges the channel to the device's sshd, half-closes included.
    async fn open_device_channel(
        &self,
        request: Request<Streaming<pb::ChannelServiceOpenDeviceChannelRequest>>,
    ) -> Result<Response<Responses>, Status> {
        use pb::channel_service_open_device_channel_request::Request as In;
        use pb::channel_service_open_device_channel_response::Response as Out;
        let mut incoming = request.into_inner();
        match incoming.next().await {
            Some(Ok(pb::ChannelServiceOpenDeviceChannelRequest {
                request: Some(In::InitialRequest(initial)),
            })) if initial.channel_device_name == SERIAL && initial.channel_profile == "ssh" => {}
            _ => return Err(Status::invalid_argument("unexpected opening")),
        }
        let socket = tokio::net::TcpStream::connect(("127.0.0.1", self.sshd_port))
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?;
        let (mut device_read, mut device_write) = socket.into_split();

        tokio::spawn(async move {
            while let Some(Ok(request)) = incoming.next().await {
                if let Some(In::SubsequentRequest(chunk)) = request.request {
                    if device_write.write_all(&chunk.channel_chunk).await.is_err() {
                        return;
                    }
                }
            }
            let _ = device_write.shutdown().await;
        });

        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(message(Out::OpenedResponse(
                    pb::ChannelServiceOpenDeviceChannelOpenedResponse {
                        channel_device_reference: Some(ResourceReference {
                            urn: format!("urn:remora:device:{SERIAL}"),
                            ..Default::default()
                        }),
                        channel_kind: "stream".into(),
                    },
                )))
                .await;
            let mut buffer = vec![0; 32 * 1024];
            loop {
                match device_read.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = pb::ChannelServiceOpenDeviceChannelChunkResponse {
                            channel_chunk: buffer[..n].to_vec(),
                        };
                        if tx.send(message(Out::ChunkResponse(chunk))).await.is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = tx
                .send(message(Out::EofResponse(
                    pb::ChannelServiceOpenDeviceChannelEofResponse {},
                )))
                .await;
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    /// Certifies the key with the user CA for `<role>@<serial>`, 15 minutes.
    async fn sign_device_ssh_certificate(
        &self,
        request: Request<pb::ChannelServiceSignDeviceSshCertificateRequest>,
    ) -> Result<Response<pb::ChannelServiceSignDeviceSshCertificateResponse>, Status> {
        let request = request.into_inner();
        let key = self.dir.join("operator.pub");
        std::fs::write(&key, &request.ssh_public_key).unwrap();
        let principal = format!("{}@{ALIAS}", request.ssh_role);
        keygen(&[
            "-s",
            self.dir.join("user_ca").to_str().unwrap(),
            "-I",
            "operator",
            "-n",
            &principal,
            "-V",
            "+15m",
            key.to_str().unwrap(),
        ]);
        let certificate = std::fs::read_to_string(self.dir.join("operator-cert.pub")).unwrap();
        let host_ca = std::fs::read_to_string(self.dir.join("host_ca.pub")).unwrap();
        Ok(Response::new(
            pb::ChannelServiceSignDeviceSshCertificateResponse {
                ssh_certificate: certificate,
                ssh_user: self.login.clone(),
                ssh_host_key_alias: ALIAS.into(),
                ssh_known_hosts: format!("@cert-authority {ALIAS} {}", host_ca.trim()),
            },
        ))
    }
}

/// A user-mode sshd that trusts the user CA for one principal, presenting
/// a host certificate for the device's alias.
fn sshd(dir: &Path, port: u16) -> std::process::Child {
    keygen(&[
        "-q",
        "-t",
        "ed25519",
        "-N",
        "",
        "-f",
        dir.join("user_ca").to_str().unwrap(),
    ]);
    keygen(&[
        "-q",
        "-t",
        "ed25519",
        "-N",
        "",
        "-f",
        dir.join("host_ca").to_str().unwrap(),
    ]);
    keygen(&[
        "-q",
        "-t",
        "ed25519",
        "-N",
        "",
        "-f",
        dir.join("host_key").to_str().unwrap(),
    ]);
    keygen(&[
        "-s",
        dir.join("host_ca").to_str().unwrap(),
        "-I",
        "device",
        "-h",
        "-n",
        ALIAS,
        dir.join("host_key.pub").to_str().unwrap(),
    ]);
    std::fs::write(dir.join("principals"), format!("user@{ALIAS}\n")).unwrap();
    let config = format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_key\nHostCertificate {d}/host_key-cert.pub\n\
         TrustedUserCAKeys {d}/user_ca.pub\nAuthorizedPrincipalsFile {d}/principals\n\
         AuthorizedKeysFile none\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
         UsePAM no\nStrictModes no\nPidFile {d}/sshd.pid\nSubsystem sftp internal-sftp\n",
        d = dir.display()
    );
    std::fs::write(dir.join("sshd_config"), config).unwrap();
    std::process::Command::new("/usr/sbin/sshd")
        .args(["-D", "-e", "-f"])
        .arg(dir.join("sshd_config"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn rmra_ssh_runs_a_command_on_a_real_sshd() {
    let available = |tool: &str| {
        std::process::Command::new(tool)
            .arg("-V")
            .stderr(Stdio::null())
            .status()
            .is_ok()
    };
    if !Path::new("/usr/sbin/sshd").exists() || !available("ssh") {
        eprintln!("skipped: no OpenSSH server/client here");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let mut server = sshd(dir.path(), port);
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let login = std::env::var("USER").unwrap_or_else(|_| "root".into());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(pb::channel_service_server::ChannelServiceServer::new(
                Gateway {
                    dir: dir.path().to_path_buf(),
                    sshd_port: port,
                    login,
                },
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
    );

    // Contexts and credentials straight on disk: login itself is covered
    // by fake_gateway.rs, and this gateway serves no IAM.
    let config = dir.path().join("rmra");
    std::fs::create_dir_all(config.join("contexts/local")).unwrap();
    std::fs::write(
        config.join("contexts/local/meta.json"),
        format!(
            r#"{{"name":"local","endpoint":{{"address":"{address}","tls":{{"disabled":true}}}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        config.join("contexts/local/credentials.json"),
        r#"{"kind":"access-key","token":"t"}"#,
    )
    .unwrap();

    let rmra = |verbose: bool| {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_rmra"));
        if verbose {
            command.arg("--verbose");
        }
        command
            .args([
                "ssh",
                SERIAL,
                "--ssh-option",
                "BatchMode=yes",
                "--",
                "echo",
                "hello-from-device",
            ])
            .env("RMRA_CONFIG", &config)
            .env_remove("RMRA_CONTEXT")
            .stdin(Stdio::null());
        command
    };
    let quiet = rmra(false).output().await.unwrap();
    let verbose = rmra(true).output().await.unwrap();

    // scp, both ways, through the same channel and pinning; -r reaches scp.
    let local = dir.path().join("payload");
    std::fs::create_dir_all(local.join("sub")).unwrap();
    std::fs::write(
        local.join("sub/file.bin"),
        (0..200_000u32).map(|i| i as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    let remote = dir.path().join("on-device");
    std::fs::create_dir_all(&remote).unwrap();
    let back = dir.path().join("back");
    let scp = |args: Vec<String>| {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_rmra"));
        command
            .arg("scp")
            .args(["--ssh-option", "BatchMode=yes"])
            .args(args)
            .env("RMRA_CONFIG", &config)
            .env_remove("RMRA_CONTEXT")
            .stdin(Stdio::null());
        command
    };
    let upload = scp(vec![
        "-r".into(),
        local.display().to_string(),
        format!("{SERIAL}:{}/", remote.display()),
    ])
    .output()
    .await
    .unwrap();
    let download = scp(vec![
        "-r".into(),
        format!("{SERIAL}:{}/payload", remote.display()),
        back.display().to_string(),
    ])
    .output()
    .await
    .unwrap();
    let _ = server.kill();
    let _ = server.wait();
    assert_eq!(
        upload.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&upload.stderr)
    );
    assert!(
        upload.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&upload.stderr)
    );
    assert_eq!(
        download.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&download.stderr)
    );
    assert_eq!(
        std::fs::read(back.join("sub/file.bin")).unwrap(),
        std::fs::read(local.join("sub/file.bin")).unwrap()
    );

    let stdout = String::from_utf8_lossy(&quiet.stdout);
    let stderr = String::from_utf8_lossy(&quiet.stderr);
    assert_eq!(
        quiet.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(stdout.trim(), "hello-from-device");
    // Silent like plain ssh: no setup chatter, and the relay finished
    // cleanly when ssh hung up on its ProxyCommand.
    assert!(stderr.trim().is_empty(), "{stderr}");

    let stderr = String::from_utf8_lossy(&verbose.stderr);
    assert_eq!(verbose.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("debug: context   local"), "{stderr}");
    assert!(stderr.contains(&format!("@{ALIAS}")), "{stderr}");
    assert!(stderr.contains("ProxyCommand="), "{stderr}");
}
