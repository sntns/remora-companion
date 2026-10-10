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

    async fn sign_device_local_ssh_certificate(
        &self,
        _: Request<pb::ChannelServiceSignDeviceLocalSshCertificateRequest>,
    ) -> Result<Response<pb::ChannelServiceSignDeviceLocalSshCertificateResponse>, Status> {
        Err(Status::unimplemented("not in this test"))
    }

    /// The code is the device, role and challenge -- normalised as the
    /// platform does -- for the test to recognise. The device maps no
    /// account to "admin".
    async fn sign_device_login_challenge(
        &self,
        request: Request<pb::ChannelServiceSignDeviceLoginChallengeRequest>,
    ) -> Result<Response<pb::ChannelServiceSignDeviceLoginChallengeResponse>, Status> {
        authorized(&request)?;
        let request = request.into_inner();
        if request.ssh_role == "admin" {
            return Err(Status::failed_precondition(
                "the device maps no account to the role admin",
            ));
        }
        let challenge: String = request
            .challenge
            .chars()
            .filter(|c| !matches!(c, '-' | ' '))
            .map(|c| c.to_ascii_uppercase())
            .collect();
        Ok(Response::new(
            pb::ChannelServiceSignDeviceLoginChallengeResponse {
                login_code: format!(
                    "{}/{}/{challenge}",
                    request.channel_device_name, request.ssh_role
                ),
            },
        ))
    }

    /// `count` codes from index 7: some were issued before.
    async fn issue_device_offline_login_codes(
        &self,
        request: Request<pb::ChannelServiceIssueDeviceOfflineLoginCodesRequest>,
    ) -> Result<Response<pb::ChannelServiceIssueDeviceOfflineLoginCodesResponse>, Status> {
        authorized(&request)?;
        let request = request.into_inner();
        Ok(Response::new(
            pb::ChannelServiceIssueDeviceOfflineLoginCodesResponse {
                login_codes: (7..7 + request.count)
                    .map(|index| pb::ChannelServiceDeviceOfflineLoginCode {
                        index,
                        login_code: format!("{}-{index:05}", request.ssh_role.to_uppercase()),
                    })
                    .collect(),
            },
        ))
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
            .env_remove("RMRA_ADDRESS")
            .env_remove("RMRA_TOKEN")
            .env_remove("RMRA_ASSUME_ROLE")
            .env_remove("RMRA_PLAINTEXT")
            .env_remove("RMRA_CA_FILE")
            .env_remove("RMRA_SERVER_NAME")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    async fn run(&self, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
        self.run_with(args, &[], stdin).await
    }

    /// Runs with `env` set on top: e.g. a context defined by the environment.
    async fn run_with(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &[u8],
    ) -> (i32, String, String) {
        let mut command = self.command(args);
        command.envs(env.iter().copied());
        let mut child = command.stdin(Stdio::piped()).spawn().unwrap();
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
    // What to run about it, as this binary spells it.
    assert!(stderr.contains("log in with `rmra --context "), "{stderr}");

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
    assert!(
        stdout.contains("\"credentials\": \"access key\""),
        "{stdout}"
    );
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
    assert!(stderr.contains("log in with `rmra --context "), "{stderr}");
}

/// An ephemeral container's or a CI job's identity: the context defined
/// whole by the environment, nothing created or logged in beforehand, and
/// nothing written to the config directory -- not even the token.
#[tokio::test]
async fn a_context_defined_by_the_environment() {
    let address = serve().await;
    let rmra = Rmra {
        config: tempfile::tempdir().unwrap(),
    };
    let env = [
        ("RMRA_ADDRESS", address.as_str()),
        ("RMRA_TOKEN", TOKEN),
        ("RMRA_PLAINTEXT", "1"),
    ];

    let (code, _, stderr) = rmra.run_with(&["whoami"], &env, b"").await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("urn:sntns:iam:local:acme:user:ada"),
        "{stderr}"
    );
    assert!(
        stderr.contains("from RMRA_ADDRESS and RMRA_TOKEN"),
        "{stderr}"
    );
    assert!(!stderr.contains(TOKEN), "the token leaked: {stderr}");

    let (code, stdout, stderr) = rmra
        .run_with(
            &["channel", "open", "525400C0FFEE", "--quiet"],
            &env,
            b"from env",
        )
        .await;
    assert_eq!((code, stdout.as_str()), (0, "vne morf"), "{stderr}");

    // Its role, a URN, assumed by every call.
    let mut as_ops = env.to_vec();
    as_ops.push(("RMRA_ASSUME_ROLE", OPS_ROLE));
    let (code, _, stderr) = rmra.run_with(&["whoami"], &as_ops, b"").await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("@ other"), "{stderr}");

    // ls shows it as what runs, never its token.
    let (code, stdout, _) = rmra
        .run_with(&["context", "ls", "--format", "json"], &env, b"")
        .await;
    assert_eq!(code, 0);
    assert!(stdout.contains("\"environment\": true"), "{stdout}");
    assert!(!stdout.contains(TOKEN), "the token leaked: {stdout}");

    // Nothing to log in or out of, no role to change, and no stored
    // context to change while it is the one that runs.
    for args in [
        &["login", "--token-stdin"][..],
        &["logout"],
        &["context", "role", "drop"],
        &["context", "create", "x", "--address", "a:1"],
    ] {
        let (code, _, stderr) = rmra.run_with(args, &env, b"token\n").await;
        assert_eq!(code, 1, "{args:?}: {stderr}");
        assert!(stderr.contains("defined for this run only"), "{stderr}");
        assert!(
            stderr.contains("unset RMRA_ADDRESS and RMRA_TOKEN"),
            "{stderr}"
        );
    }

    let leftovers: Vec<_> = std::fs::read_dir(rmra.config.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(leftovers.is_empty(), "written to the config: {leftovers:?}");

    // Half an environment, or one that also names a context: usage errors.
    let (code, _, stderr) = rmra.run_with(&["whoami"], &env[1..], b"").await;
    assert_eq!(code, 2);
    assert!(
        stderr.contains("RMRA_TOKEN is set without RMRA_ADDRESS"),
        "{stderr}"
    );
    let mut named = env.to_vec();
    named.push(("RMRA_CONTEXT", "local"));
    let (code, _, stderr) = rmra.run_with(&["whoami"], &named, b"").await;
    assert_eq!(code, 2);
    assert!(stderr.contains("unset one or the other"), "{stderr}");

    // --context names a stored context over the environment's.
    let (code, _, stderr) = rmra
        .run_with(
            &[
                "-c",
                "local",
                "context",
                "create",
                "local",
                "--address",
                &address,
                "--plaintext",
            ],
            &env,
            b"",
        )
        .await;
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = rmra.run_with(&["-c", "local", "whoami"], &env, b"").await;
    assert_eq!(code, 1);
    assert!(stderr.contains("not logged in"), "{stderr}");
}

#[tokio::test]
async fn console_login_codes() {
    let address = serve().await;
    let rmra = Rmra {
        config: tempfile::tempdir().unwrap(),
    };
    let env = [
        ("RMRA_ADDRESS", address.as_str()),
        ("RMRA_TOKEN", TOKEN),
        ("RMRA_PLAINTEXT", "1"),
    ];

    // The code alone on stdout, the challenge sent as typed.
    let (code, stdout, stderr) = rmra
        .run_with(
            &["local", "login-code", "525400C0FFEE", "k7qm-3xrb"],
            &env,
            b"",
        )
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "525400C0FFEE/user/K7QM3XRB\n");

    // The platform's refusal, in its words.
    let (code, _, stderr) = rmra
        .run_with(
            &[
                "local",
                "login-code",
                "525400C0FFEE",
                "--role",
                "admin",
                "K7QM-3XRB",
            ],
            &env,
            b"",
        )
        .await;
    assert_eq!(code, 1);
    assert!(
        stderr.contains("maps no account to the role admin"),
        "{stderr}"
    );

    let (code, stdout, stderr) = rmra
        .run_with(
            &[
                "local",
                "offline-codes",
                "525400C0FFEE",
                "--role",
                "user",
                "--count",
                "3",
                "--format",
                "json",
            ],
            &env,
            b"",
        )
        .await;
    assert_eq!(code, 0, "{stderr}");
    let codes: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        codes,
        serde_json::json!([
            {"index": 7, "code": "USER-00007"},
            {"index": 8, "code": "USER-00008"},
            {"index": 9, "code": "USER-00009"},
        ])
    );

    // More than the platform issues at once is refused before asking.
    let (code, _, stderr) = rmra
        .run_with(
            &["local", "offline-codes", "525400C0FFEE", "--count", "101"],
            &env,
            b"",
        )
        .await;
    assert_eq!(code, 2, "{stderr}");
}

/// `rmra local console` on a terminal of its own (a pseudo-terminal), the
/// device's console on another: the challenge the device shows is answered
/// with the platform's code, typed once the password prompt is up, and
/// C-a x hands the terminal back.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_console_answers_the_login_challenge() {
    use std::os::fd::{AsFd, OwnedFd};

    use nix::{
        poll::{poll, PollFd, PollFlags, PollTimeout},
        pty::{openpty, Winsize},
        sys::termios,
    };

    /// What `fd` says until `needle` shows, or panics after 10 s.
    fn read_until(fd: &OwnedFd, needle: &[u8]) -> Vec<u8> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = Vec::new();
        while !seen.windows(needle.len()).any(|w| w == needle) {
            assert!(
                std::time::Instant::now() < deadline,
                "{:?} never came: {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&seen)
            );
            let mut fds = [PollFd::new(fd.as_fd(), PollFlags::POLLIN)];
            if poll(&mut fds, PollTimeout::from(100u16)).unwrap() > 0 {
                let mut buffer = [0u8; 4096];
                match nix::unistd::read(fd, &mut buffer) {
                    Ok(n) => seen.extend_from_slice(&buffer[..n]),
                    Err(_) => break,
                }
            }
        }
        seen
    }

    let address = serve().await;
    let rmra = Rmra {
        config: tempfile::tempdir().unwrap(),
    };

    let device = openpty(None, None).unwrap();
    let mut raw = termios::tcgetattr(&device.slave).unwrap();
    termios::cfmakeraw(&mut raw);
    termios::tcsetattr(&device.slave, termios::SetArg::TCSANOW, &raw).unwrap();
    let port = nix::unistd::ttyname(&device.slave).unwrap();

    let size = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let terminal = openpty(Some(&size), None).unwrap();
    let mut command = rmra.command(&["local", "console", port.to_str().unwrap(), "--role", "user"]);
    command
        .envs([
            ("RMRA_ADDRESS", address.as_str()),
            ("RMRA_TOKEN", TOKEN),
            ("RMRA_PLAINTEXT", "1"),
            ("TERM", "xterm"),
        ])
        .stdin(Stdio::from(terminal.slave.try_clone().unwrap()))
        .stdout(Stdio::from(terminal.slave.try_clone().unwrap()))
        .stderr(Stdio::from(terminal.slave.try_clone().unwrap()));
    let mut child = command.spawn().unwrap();
    drop(terminal.slave);
    let terminal = terminal.master;
    let terminal = tokio::task::spawn_blocking(move || {
        read_until(&terminal, b"login: user");
        terminal
    })
    .await
    .unwrap();

    let master = device.master;
    nix::unistd::write(
        &master,
        b"E2ETEST0002 login: root\r\n\
          Remora local login: root@525400C0FFEE\r\n\
          Challenge: k7qm-3xrb\r\n\
          Type the code for this challenge (or an offline code) at the password prompt.\r\n",
    )
    .unwrap();
    let master = tokio::task::spawn_blocking(move || {
        // Nothing is typed before the password prompt...
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut fds = [PollFd::new(master.as_fd(), PollFlags::POLLIN)];
        assert_eq!(poll(&mut fds, PollTimeout::ZERO).unwrap(), 0);
        // ...and the code is, once it is up.
        nix::unistd::write(&master, b"Password: ").unwrap();
        let typed = read_until(&master, b"\r");
        assert_eq!(typed, b"525400C0FFEE/user/K7QM3XRB\r");
        master
    })
    .await
    .unwrap();
    let terminal = tokio::task::spawn_blocking(move || {
        read_until(&terminal, b"code typed: root@525400C0FFEE as user");
        nix::unistd::write(&terminal, b"\x01x").unwrap();
        terminal
    })
    .await
    .unwrap();

    let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
        .await
        .expect("C-a x did not end the console")
        .unwrap();
    assert!(status.success());
    drop((master, terminal));
}
