//! The shipped binary against an in-process fake sntns-platform gateway:
//! login is verified and stored, whoami reads it back, and `channel open`
//! carries stdin to the device and the device's answer to stdout -- with
//! the end of stdin arriving as a half-close the device still answers.

use std::{pin::Pin, process::Stdio};

use remora_platform_grpc::sntns::service::{
    iam::v1 as iam, remorachannel::v1 as pb, v1::ResourceReference,
};
use tokio::{io::AsyncWriteExt, sync::mpsc};
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

const TOKEN: &str = "operator-token";

/// The one role the login may assume: in another tenant, "other".
const OPS_ROLE: &str = "urn:sntns:iam:local:other:role:ops";

/// Authenticates the call and says which tenant it acts in: the login's
/// own ("acme"), or, with an assume-role header, the role's -- as the
/// platform does, refusing a role the login may not assume.
fn authorized<T>(request: &Request<T>) -> Result<&'static str, Status> {
    match request.metadata().get("authentication-token") {
        Some(token) if token == TOKEN => {}
        _ => return Err(Status::unauthenticated("invalid access key")),
    }
    match request
        .metadata()
        .get("assume-role")
        .map(|v| v.to_str().unwrap_or_default())
    {
        None => Ok("acme"),
        Some(OPS_ROLE) => Ok("other"),
        Some(_) => Err(Status::permission_denied("role permission denied")),
    }
}

struct Iam;

#[tonic::async_trait]
impl iam::user_service_server::UserService for Iam {
    async fn get_current_user(
        &self,
        request: Request<iam::UserServiceGetCurrentUserRequest>,
    ) -> Result<Response<iam::UserServiceGetCurrentUserResponse>, Status> {
        // An assumed role's principal is no user.
        if authorized(&request)? != "acme" {
            return Err(Status::not_found("user not found"));
        }
        Ok(Response::new(iam::UserServiceGetCurrentUserResponse {
            user_descriptor: Some(iam::UserDescriptor {
                resource: Some(ResourceReference {
                    urn: "urn:sntns:iam:local:acme:user:ada".into(),
                    name: "ada".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        }))
    }
}

#[tonic::async_trait]
impl iam::account_service_server::AccountService for Iam {
    async fn get_current_account(
        &self,
        request: Request<iam::AccountServiceGetCurrentAccountRequest>,
    ) -> Result<Response<iam::AccountServiceGetCurrentAccountResponse>, Status> {
        let tenant = authorized(&request)?;
        Ok(Response::new(
            iam::AccountServiceGetCurrentAccountResponse {
                account_descriptor: Some(iam::AccountDescriptor {
                    resource: Some(ResourceReference {
                        name: tenant.into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            },
        ))
    }
}

/// A device that reads everything until the client half-closes, then
/// answers with what it read, reversed -- so the answer can only arrive if
/// the half-close did not end the channel.
struct Channels;

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
impl pb::channel_service_server::ChannelService for Channels {
    type OpenDeviceChannelStream = Responses;

    async fn open_device_channel(
        &self,
        request: Request<Streaming<pb::ChannelServiceOpenDeviceChannelRequest>>,
    ) -> Result<Response<Responses>, Status> {
        use pb::channel_service_open_device_channel_request::Request as In;
        use pb::channel_service_open_device_channel_response::Response as Out;
        let tenant = authorized(&request)?;
        let mut incoming = request.into_inner();
        let Some(Ok(pb::ChannelServiceOpenDeviceChannelRequest {
            request: Some(In::InitialRequest(initial)),
        })) = incoming.next().await
        else {
            return Err(Status::invalid_argument("no initial request"));
        };
        if initial.channel_profile != "ssh" {
            return Err(Status::invalid_argument("unknown profile"));
        }
        if initial.channel_device_name == "DROP" {
            // Opens, then goes away mid-channel, as a device rebooting does.
            let (tx, rx) = mpsc::channel(4);
            tokio::spawn(async move {
                let _ = tx
                    .send(message(Out::OpenedResponse(
                        pb::ChannelServiceOpenDeviceChannelOpenedResponse {
                            channel_device_reference: None,
                            channel_kind: "stream".into(),
                        },
                    )))
                    .await;
                let _ = incoming.next().await;
                let _ = tx
                    .send(Err(Status::unavailable("the device went away")))
                    .await;
            });
            return Ok(Response::new(Box::pin(ReceiverStream::new(rx))));
        }
        // A device of the other tenant: only reachable as its role.
        if initial.channel_device_name == "OTHERDEV" && tenant != "other" {
            return Err(Status::not_found("device not found"));
        }
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(message(Out::OpenedResponse(
                    pb::ChannelServiceOpenDeviceChannelOpenedResponse {
                        channel_device_reference: Some(ResourceReference {
                            urn: format!("urn:remora:device:{}", initial.channel_device_name),
                            ..Default::default()
                        }),
                        channel_kind: "stream".into(),
                    },
                )))
                .await;
            let mut received = Vec::new();
            while let Some(Ok(request)) = incoming.next().await {
                if let Some(In::SubsequentRequest(chunk)) = request.request {
                    received.extend(chunk.channel_chunk);
                }
            }
            received.reverse();
            let _ = tx
                .send(message(Out::ChunkResponse(
                    pb::ChannelServiceOpenDeviceChannelChunkResponse {
                        channel_chunk: received,
                    },
                )))
                .await;
            let _ = tx
                .send(message(Out::EofResponse(
                    pb::ChannelServiceOpenDeviceChannelEofResponse {},
                )))
                .await;
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn sign_device_ssh_certificate(
        &self,
        _: Request<pb::ChannelServiceSignDeviceSshCertificateRequest>,
    ) -> Result<Response<pb::ChannelServiceSignDeviceSshCertificateResponse>, Status> {
        Err(Status::unimplemented("not in this test"))
    }
}

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(iam::user_service_server::UserServiceServer::new(Iam))
            .add_service(iam::account_service_server::AccountServiceServer::new(Iam))
            .add_service(pb::channel_service_server::ChannelServiceServer::new(
                Channels,
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
    );
    address
}

struct Rmra {
    config: tempfile::TempDir,
}

impl Rmra {
    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_rmra"));
        command
            .args(args)
            .env("RMRA_CONFIG", self.config.path())
            .env_remove("RMRA_CONTEXT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    async fn run(&self, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
        let mut child = self.command(args).stdin(Stdio::piped()).spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(stdin).await.unwrap();
        drop(input);
        let output = child.wait_with_output().await.unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

#[tokio::test]
async fn login_whoami_and_a_channel_on_stdio() {
    let address = serve().await;
    let rmra = Rmra {
        config: tempfile::tempdir().unwrap(),
    };

    let (code, _, stderr) = rmra
        .run(
            &[
                "context",
                "create",
                "local",
                "--address",
                &address,
                "--plaintext",
                "--use",
            ],
            b"",
        )
        .await;
    assert_eq!(code, 0, "{stderr}");

    // Refused credentials are not stored.
    let (code, _, stderr) = rmra.run(&["login", "--token-stdin"], b"wrong\n").await;
    assert_eq!(code, 1);
    assert!(stderr.contains("invalid access key"), "{stderr}");
    let (code, _, stderr) = rmra.run(&["whoami"], b"").await;
    assert_eq!(code, 1);
    assert!(stderr.contains("not logged in"), "{stderr}");

    let (code, _, stderr) = rmra
        .run(&["login", "--token-stdin"], format!("{TOKEN}\n").as_bytes())
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("ada"), "{stderr}");

    let (code, _, stderr) = rmra.run(&["whoami"], b"").await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("urn:sntns:iam:local:acme:user:ada"),
        "{stderr}"
    );

    let (code, stdout, _) = rmra.run(&["context", "ls", "--format", "json"], b"").await;
    assert_eq!(code, 0);
    assert!(stdout.contains("\"login\": \"access key\""), "{stdout}");
    assert!(
        !stdout.contains(TOKEN),
        "a secret leaked into context ls: {stdout}"
    );

    // The device only answers after our end of input: stdout gets the
    // answer, and nothing else.
    let (code, stdout, stderr) = rmra
        .run(
            &["channel", "open", "525400C0FFEE", "--quiet"],
            b"hello device",
        )
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "ecived olleh");
    assert!(stderr.is_empty(), "--quiet printed: {stderr}");

    // A channel lost mid-session, as ssh's ProxyCommand sees it: one plain
    // line saying why, with carriage returns for ssh's raw-mode terminal --
    // no decoration, no repeated causes.
    let (code, stdout, stderr) = rmra
        .run(&["channel", "open", "DROP", "--quiet"], b"keystrokes")
        .await;
    assert_eq!(code, 255);
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        "\r\nrmra: the connection to DROP was lost: the gateway ended the channel: \
         Unavailable: the device went away\r\n"
    );

    // Roles: another tenant's role, remembered, verified, then assumed by
    // the context -- every command, the ssh ProxyCommand's channel open
    // included.
    let (code, _, stderr) = rmra
        .run(
            &[
                "context",
                "role",
                "add",
                "ops",
                "urn:sntns:iam:local:other:role:ops",
            ],
            b"",
        )
        .await;
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = rmra
        .run(
            &[
                "context",
                "role",
                "assume",
                "urn:sntns:iam:local:other:role:admin",
            ],
            b"",
        )
        .await;
    assert_eq!(code, 1);
    assert!(stderr.contains("role permission denied"), "{stderr}");
    let (code, _, stderr) = rmra.run(&["context", "role", "assume", "ops"], b"").await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("other"), "{stderr}");

    let (code, _, stderr) = rmra.run(&["whoami"], b"").await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("acting as") && stderr.contains("ops @ other"),
        "{stderr}"
    );
    let (_, stdout, _) = rmra.run(&["context", "inspect"], b"").await;
    assert!(stdout.contains("\"assumedRole\": \"ops\""), "{stdout}");

    let (code, stdout, stderr) = rmra
        .run(&["channel", "open", "OTHERDEV", "--quiet"], b"as the role")
        .await;
    assert_eq!((code, stdout.as_str()), (0, "elor eht sa"), "{stderr}");
    // The role is the context's, never a command's; the context is chosen
    // before the command, never after it.
    let (code, _, _) = rmra
        .run(
            &["--no-assume-role", "channel", "open", "OTHERDEV", "--quiet"],
            b"",
        )
        .await;
    assert_eq!(code, 2);
    let (code, _, _) = rmra
        .run(&["channel", "open", "OTHERDEV", "-c", "local"], b"")
        .await;
    assert_eq!(code, 2);

    let (code, _, _) = rmra.run(&["context", "role", "drop"], b"").await;
    assert_eq!(code, 0);
    let (code, _, stderr) = rmra
        .run(&["channel", "open", "OTHERDEV", "--quiet"], b"")
        .await;
    assert_eq!(code, 255);
    assert!(stderr.contains("device not found"), "{stderr}");

    // The role as part of the context, decided at login: a context created
    // to act as another tenant's role, verified when it logs in; a refused
    // role refuses the whole login and changes nothing.
    let (code, _, stderr) = rmra
        .run(
            &[
                "context",
                "create",
                "other-ops",
                "--address",
                &address,
                "--plaintext",
                "--assume-role",
                OPS_ROLE,
            ],
            b"",
        )
        .await;
    assert_eq!(code, 0, "{stderr}");
    let login = format!("{TOKEN}\n");
    let (code, _, stderr) = rmra
        .run(
            &["-c", "other-ops", "login", "--token-stdin"],
            login.as_bytes(),
        )
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("acting as") && stderr.contains("@ other"),
        "{stderr}"
    );
    let (code, _, stderr) = rmra
        .run(
            &[
                "-c",
                "other-ops",
                "login",
                "--assume-role",
                "urn:sntns:iam:local:other:role:admin",
                "--token-stdin",
            ],
            login.as_bytes(),
        )
        .await;
    assert_eq!(code, 1);
    assert!(stderr.contains("role permission denied"), "{stderr}");
    let (_, stdout, _) = rmra
        .run(&["-c", "other-ops", "context", "inspect"], b"")
        .await;
    assert!(
        stdout.contains(OPS_ROLE),
        "a refused login changed the role: {stdout}"
    );
    let (code, _, stderr) = rmra
        .run(
            &[
                "-c",
                "other-ops",
                "login",
                "--no-assume-role",
                "--token-stdin",
            ],
            login.as_bytes(),
        )
        .await;
    assert_eq!(code, 0, "{stderr}");
    let (_, stdout, _) = rmra
        .run(&["-c", "other-ops", "context", "inspect"], b"")
        .await;
    assert!(!stdout.contains("assumedRole"), "{stdout}");

    // A base context declined per role: `context create --from`, sharing
    // the base's single login; a refused role creates nothing.
    let (code, _, stderr) = rmra
        .run(
            &[
                "context",
                "create",
                "as-admin",
                "--from",
                "local",
                "--assume-role",
                "urn:sntns:iam:local:other:role:admin",
            ],
            b"",
        )
        .await;
    assert_eq!(code, 1);
    assert!(stderr.contains("role permission denied"), "{stderr}");
    let (code, _, stderr) = rmra
        .run(
            &[
                "context",
                "create",
                "as-ops",
                "--from",
                "local",
                "--assume-role",
                "ops",
            ],
            b"",
        )
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("acting as") && stderr.contains("@ other"),
        "{stderr}"
    );
    let (code, stdout, stderr) = rmra
        .run(
            &["-c", "as-ops", "channel", "open", "OTHERDEV", "--quiet"],
            b"declined",
        )
        .await;
    assert_eq!((code, stdout.as_str()), (0, "denilced"), "{stderr}");
    let (_, stdout, _) = rmra.run(&["context", "ls"], b"").await;
    assert!(stdout.contains("via local"), "{stdout}");

    // Renamed, it keeps its login and role.
    let (code, _, stderr) = rmra
        .run(&["context", "rename", "as-ops", "tenant-ops"], b"")
        .await;
    assert_eq!(code, 0, "{stderr}");
    let (code, stdout, stderr) = rmra
        .run(
            &["-c", "tenant-ops", "channel", "open", "OTHERDEV", "--quiet"],
            b"renamed",
        )
        .await;
    assert_eq!((code, stdout.as_str()), (0, "demaner"), "{stderr}");

    let (code, _, stderr) = rmra.run(&["context", "rm", "-f", "local"], b"").await;
    assert_eq!(code, 1);
    assert!(stderr.contains("tenant-ops"), "{stderr}");
    let (code, _, _) = rmra.run(&["context", "rm", "-f", "tenant-ops"], b"").await;
    assert_eq!(code, 0);

    let (code, _, _) = rmra.run(&["logout"], b"").await;
    assert_eq!(code, 0);
    let (code, _, stderr) = rmra.run(&["channel", "open", "525400C0FFEE"], b"").await;
    assert_eq!(code, 255);
    assert!(stderr.contains("not logged in"), "{stderr}");
}
