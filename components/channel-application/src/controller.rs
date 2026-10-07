use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use error_stack::{Report, ResultExt};
use remora_channel::{
    adapter::{
        gateway::{Channel, ChannelGatewayAdapterService},
        ssh::SshClientAdapterService,
    },
    application::{ChannelServiceInterface, Error, Result},
    model::{
        ChannelKind, ExecOutcome, ExecRequest, PreparedSsh, ProxyCommandBuilder, ProxyTarget,
        ScpRequest, SshCertificate, SshCommand, SshKeys, SshRequest, SshRole,
    },
};
use remora_context::{
    application::ContextService,
    model::{ContextOverride, ResolvedContext},
};
use remora_progress::OperationContext;
use ssh_key::{rand_core::OsRng, Algorithm, LineEnding, PrivateKey};

use crate::arguments::{self, Operand};

/// The channel vertical's use case: the gateway port for channels and
/// certificates, the ssh-client port to run ssh, and the context vertical's
/// application port to know which platform, as whom.
pub struct ChannelControllerImpl {
    contexts: ContextService,
    gateway: ChannelGatewayAdapterService,
    ssh: SshClientAdapterService,
    /// Keys certified for `exec`, by context override, device and role,
    /// with when: reused while their certificate is fresh.
    certified: tokio::sync::Mutex<HashMap<CertifiedFor, (Arc<Session>, Instant)>>,
}

/// What a key certified for `exec` was certified for: the context override
/// (empty for the current context), the device and the ssh role.
type CertifiedFor = (String, String, SshRole);

/// How long a key certified for `exec` is reused: well within its
/// certificate's 15 minutes, which only has to hold when a connection
/// authenticates.
const REUSE_CERTIFIED: Duration = Duration::from_secs(10 * 60);

impl ChannelControllerImpl {
    pub fn new(
        contexts: ContextService,
        gateway: ChannelGatewayAdapterService,
        ssh: SshClientAdapterService,
    ) -> Self {
        Self {
            contexts,
            gateway,
            ssh,
            certified: Default::default(),
        }
    }

    /// A session certified for `request`'s device and role: the one
    /// certified last for them, while fresh, else a new one.
    async fn exec_session(&self, request: &ExecRequest) -> Result<Arc<Session>> {
        let key = (
            request
                .over
                .as_ref()
                .map(|over| over.name.clone())
                .unwrap_or_default(),
            request.device.clone(),
            request.role,
        );
        let mut certified = self.certified.lock().await;
        if let Some((session, at)) = certified.get(&key) {
            if at.elapsed() < REUSE_CERTIFIED {
                return Ok(session.clone());
            }
        }
        let session = Arc::new(
            self.certify(
                request.over.as_ref(),
                &request.device,
                request.role,
                &request.proxy_command,
                vec!["BatchMode=yes".to_owned()],
            )
            .await?,
        );
        certified.insert(key, (session.clone(), Instant::now()));
        Ok(session)
    }

    /// What every ssh-family session needs: a throwaway ed25519 key,
    /// certified for `role` on `device`, and the options that pin ssh to the
    /// device's channel, host authority and that certificate -- followed by
    /// the caller's own `-o` options, which come after so ssh (first value
    /// wins) never lets them loosen the pinning. The key material's own
    /// options are the ssh client port's, which writes it.
    async fn certify(
        &self,
        over: Option<&ContextOverride>,
        device: &str,
        role: SshRole,
        proxy_command: &ProxyCommandBuilder,
        options: Vec<String>,
    ) -> Result<Session> {
        let context = self.resolve(over).await?;

        let key =
            PrivateKey::random(&mut OsRng, Algorithm::Ed25519).change_context(Error::Keygen)?;
        let public_key = key
            .public_key()
            .to_openssh()
            .change_context(Error::Keygen)?;
        let private_pem = key
            .to_openssh(LineEnding::LF)
            .change_context(Error::Keygen)?;

        let certified = self
            .gateway
            .sign_ssh_certificate(&context, device, &public_key, role)
            .await
            .change_context_lazy(|| Error::Certify(device.to_owned()))?;

        let proxy_command = proxy_command(&ProxyTarget {
            context: &context.context.name,
            device,
        });
        let mut pinning = vec![
            "-o".to_owned(),
            format!("ProxyCommand={proxy_command}"),
            "-o".to_owned(),
            "IdentitiesOnly=yes".to_owned(),
            "-o".to_owned(),
            format!("GlobalKnownHostsFile={}", null_device()),
            "-o".to_owned(),
            "StrictHostKeyChecking=yes".to_owned(),
            "-o".to_owned(),
            format!("HostKeyAlias={}", certified.host_key_alias),
        ];
        for option in options {
            pinning.extend(["-o".to_owned(), option]);
        }
        let keys = SshKeys::new(
            private_pem,
            certified.certificate.clone(),
            certified.known_hosts.clone(),
        );
        Ok(Session {
            context: context.context.name,
            certified,
            pinning,
            keys,
        })
    }

    async fn resolve(&self, over: Option<&ContextOverride>) -> Result<ResolvedContext> {
        self.contexts.resolve(over).await.map_err(|report| {
            let message = report.current_context().to_string();
            report.change_context(Error::Context(message))
        })
    }
}

#[async_trait::async_trait]
impl ChannelServiceInterface for ChannelControllerImpl {
    async fn open(
        &self,
        over: Option<&ContextOverride>,
        device: &str,
        profile: &str,
    ) -> Result<Channel> {
        let context = self.resolve(over).await?;
        let channel = self
            .gateway
            .open(&context, device, profile)
            .await
            .change_context_lazy(|| Error::Open(device.to_owned()))?;
        // Which kind a profile is, only the device knows; a datagram channel
        // is closed (dropped) as soon as it is known to be one.
        if channel.opened.kind == ChannelKind::Datagram {
            return Err(Report::new(Error::Datagram(profile.to_owned())));
        }
        Ok(channel)
    }

    async fn prepare_ssh(&self, request: SshRequest) -> Result<PreparedSsh> {
        // Checked before anything is certified, so a refused option costs
        // nothing.
        let split = arguments::split(&request.arguments)
            .map_err(|reason| Report::new(Error::RefusedArgument(reason)))?;
        check_options(&request.options)?;

        let session = self
            .certify(
                request.over.as_ref(),
                &request.device,
                request.role,
                &request.proxy_command,
                request.options,
            )
            .await?;
        let mut arguments = session.pinning;
        arguments.extend(split.options);
        let login = split
            .login
            .or(request.login)
            .unwrap_or_else(|| session.certified.user.clone());
        arguments.push(format!("{login}@{}", session.certified.host_key_alias));
        arguments.extend(split.command);

        Ok(PreparedSsh {
            context: session.context,
            device: request.device,
            login,
            host_key_alias: session.certified.host_key_alias,
            role: request.role,
            command: SshCommand {
                program: request.binary,
                arguments,
            },
            keys: session.keys,
        })
    }

    async fn prepare_scp(&self, request: ScpRequest) -> Result<PreparedSsh> {
        let split = arguments::split_scp(&request.arguments)
            .map_err(|reason| Report::new(Error::RefusedArgument(reason)))?;
        check_options(&request.options)?;

        // One certificate is for one device: every remote operand must be it.
        let operands_error = |message: String| Report::new(Error::Operands(message));
        if split.operands.len() < 2 {
            return Err(operands_error(
                "scp needs a source and a destination, e.g. ./file DEVICE:/tmp/".into(),
            ));
        }
        let mut device: Option<String> = None;
        let mut user: Option<String> = None;
        for operand in &split.operands {
            if let Operand::Remote { user: u, host, .. } = operand {
                match &device {
                    Some(seen) if !seen.eq_ignore_ascii_case(host) => {
                        return Err(operands_error(format!(
                            "{seen} and {host}: one scp copies to or from one device at a time"
                        )))
                    }
                    Some(_) => {}
                    None => device = Some(host.clone()),
                }
                match (&user, u) {
                    (Some(seen), Some(u)) if seen != u => {
                        return Err(operands_error(format!(
                            "{seen}@ and {u}@: one scp logs in as one account"
                        )))
                    }
                    (None, Some(u)) => user = Some(u.clone()),
                    _ => {}
                }
            }
        }
        let device = device.ok_or_else(|| {
            operands_error("no remote operand: write the device side as DEVICE:path".into())
        })?;

        let session = self
            .certify(
                request.over.as_ref(),
                &device,
                request.role,
                &request.proxy_command,
                request.options,
            )
            .await?;
        let login = user
            .or(request.login)
            .unwrap_or_else(|| session.certified.user.clone());
        let alias = session.certified.host_key_alias.clone();
        let mut arguments = session.pinning;
        arguments.extend(split.options);
        // Operands start after `--`, so a local file named like an option
        // can't be read as one.
        arguments.push("--".to_owned());
        arguments.extend(split.operands.into_iter().map(|operand| match operand {
            Operand::Local(path) => path,
            Operand::Remote { path, .. } => format!("{login}@{alias}:{path}"),
        }));

        Ok(PreparedSsh {
            context: session.context,
            device,
            login,
            host_key_alias: alias,
            role: request.role,
            command: SshCommand {
                program: request.binary,
                arguments,
            },
            keys: session.keys,
        })
    }

    async fn run_ssh(&self, prepared: PreparedSsh) -> Result<i32> {
        self.ssh
            .run(&prepared.command, &prepared.keys)
            .await
            .change_context(Error::Ssh)
    }

    async fn exec(&self, request: ExecRequest, ctx: &OperationContext) -> Result<ExecOutcome> {
        let session = self.exec_session(&request).await?;
        let login = request
            .login
            .clone()
            .unwrap_or_else(|| session.certified.user.clone());
        let mut arguments = session.pinning.clone();
        arguments.push("-T".to_owned());
        arguments.push(format!("{login}@{}", session.certified.host_key_alias));
        arguments.push(request.command.clone());
        let command = SshCommand {
            program: request.binary.clone(),
            arguments,
        };
        self.ssh
            .exec(
                &command,
                &session.keys,
                request.input.as_ref(),
                request.echo,
                ctx,
            )
            .await
            .map_err(|report| {
                if matches!(
                    report.current_context(),
                    remora_channel::adapter::ssh::Error::Cancelled
                ) {
                    report.change_context(Error::Cancelled)
                } else {
                    report.change_context(Error::Ssh)
                }
            })
    }
}

/// A certified session before its client-specific arguments.
struct Session {
    context: String,
    certified: SshCertificate,
    pinning: Vec<String>,
    keys: SshKeys,
}

fn check_options(options: &[String]) -> Result<()> {
    for option in options {
        arguments::check_option(option)
            .map_err(|reason| Report::new(Error::RefusedArgument(reason)))?;
    }
    Ok(())
}

fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{Arc, Mutex},
    };

    use remora_channel::model::ScpRequest;
    use remora_channel::{
        adapter::gateway::{self, ChannelGatewayAdapter},
        model::{SshCertificate, SshRole},
    };
    use remora_channel_adapter_openssh::OpenSshClientImpl;
    use remora_context::{
        adapter::{
            credentials::CredentialStoreAdapterService,
            platform::{self, PlatformSessionAdapter, PlatformSessionAdapterService},
            store::ContextStoreAdapterService,
        },
        model::{Context, Credentials, Endpoint, Principal, Secret, Tls},
    };
    use remora_context_adapter_file::{FileContextStoreImpl, FileCredentialStoreImpl};
    use remora_context_application::ContextControllerImpl;

    use super::*;

    struct Platform;

    #[async_trait::async_trait]
    impl PlatformSessionAdapter for Platform {
        async fn whoami(&self, _: &Context, _: &Credentials) -> platform::Result<Principal> {
            Ok(Principal {
                user_urn: "urn:user:ada".into(),
                user_name: "ada".into(),
                account_name: None,
            })
        }

        async fn acting_account(
            &self,
            _: &Context,
            _: &Credentials,
            _: &str,
        ) -> platform::Result<Option<String>> {
            Ok(None)
        }
    }

    /// Certifies anything, remembering which key and role it was asked for.
    #[derive(Default, Clone)]
    struct Gateway(Arc<Mutex<Vec<(String, SshRole)>>>);

    #[async_trait::async_trait]
    impl ChannelGatewayAdapter for Gateway {
        /// Only a "datagram" profile opens, as a channel that carries
        /// nothing.
        async fn open(
            &self,
            _: &ResolvedContext,
            device: &str,
            profile: &str,
        ) -> gateway::Result<Channel> {
            if profile != "datagram" {
                return Err(Report::new(gateway::Error::NotConnected));
            }
            Ok(Channel {
                opened: remora_channel::model::OpenedChannel {
                    device_urn: format!("urn:device:{device}"),
                    kind: ChannelKind::Datagram,
                },
                sender: Box::new(Nothing),
                receiver: Box::new(Nothing),
            })
        }

        async fn sign_ssh_certificate(
            &self,
            _: &ResolvedContext,
            device: &str,
            public_key: &str,
            role: SshRole,
        ) -> gateway::Result<SshCertificate> {
            self.0.lock().unwrap().push((public_key.to_owned(), role));
            Ok(SshCertificate {
                certificate: format!("ssh-ed25519-cert-v01@openssh.com CERT {device}"),
                user: "operator".into(),
                host_key_alias: device.to_lowercase(),
                known_hosts: "@cert-authority *.devices ssh-ed25519 HOSTCA".into(),
            })
        }
    }

    struct Nothing;

    #[async_trait::async_trait]
    impl gateway::ChannelSender for Nothing {
        async fn send(&mut self, _: Vec<u8>) -> gateway::Result<()> {
            Ok(())
        }
        async fn close_write(&mut self) -> gateway::Result<()> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl gateway::ChannelReceiver for Nothing {
        async fn recv(&mut self) -> gateway::Result<Option<Vec<u8>>> {
            Ok(None)
        }
    }

    async fn controller(root: &Path, gateway: Gateway) -> ChannelControllerImpl {
        let contexts = ContextControllerImpl::new(
            ContextStoreAdapterService::new(FileContextStoreImpl::new(root)),
            CredentialStoreAdapterService::new(FileCredentialStoreImpl::new(root)),
            PlatformSessionAdapterService::new(Platform),
        );
        use remora_context::application::ContextServiceInterface;
        contexts
            .create(
                Context {
                    name: "eu2".into(),
                    description: None,
                    endpoint: Endpoint {
                        address: "gateway:50051".into(),
                        tls: Tls::default(),
                    },
                    roles: Default::default(),
                    assumed_role: None,
                    login: None,
                },
                remora_context::model::RoleOverride::Keep,
                false,
            )
            .await
            .unwrap();
        contexts
            .login(
                None,
                Credentials {
                    secret: Secret::AccessKey { token: "t".into() },
                },
                remora_context::model::RoleOverride::Keep,
            )
            .await
            .unwrap();
        ChannelControllerImpl::new(
            ContextService::new(contexts),
            ChannelGatewayAdapterService::new(gateway),
            SshClientAdapterService::new(OpenSshClientImpl),
        )
    }

    fn request(arguments: &[&str], binary: &str) -> SshRequest {
        SshRequest {
            over: None,
            device: "525400C0FFEE".into(),
            role: SshRole::Admin,
            login: None,
            options: vec!["ServerAliveInterval=5".into()],
            arguments: arguments.iter().map(|s| s.to_string()).collect(),
            binary: binary.into(),
            proxy_command: Arc::new(|target| {
                format!(
                    "rmra --context {} channel open {}",
                    target.context, target.device
                )
            }),
        }
    }

    #[tokio::test]
    async fn prepares_a_pinned_session_with_a_certified_throwaway_key() {
        let root = tempfile::tempdir().unwrap();
        let gateway = Gateway::default();
        let channel = controller(root.path(), gateway.clone()).await;

        let prepared = channel
            .prepare_ssh(request(&["-t", "uptime"], "ssh"))
            .await
            .unwrap();
        assert_eq!(prepared.context, "eu2");
        assert_eq!(prepared.login, "operator");
        let args = &prepared.command.arguments;
        assert_eq!(
            args[1],
            "ProxyCommand=rmra --context eu2 channel open 525400C0FFEE"
        );
        assert!(args.contains(&"StrictHostKeyChecking=yes".to_owned()));
        assert!(args.contains(&"HostKeyAlias=525400c0ffee".to_owned()));
        // Pinning first, then the caller's -o, then passthrough, then the
        // destination and the command.
        let pinned = args
            .iter()
            .position(|a| a.starts_with("HostKeyAlias="))
            .unwrap();
        let caller = args
            .iter()
            .position(|a| a == "ServerAliveInterval=5")
            .unwrap();
        assert!(pinned < caller);
        assert_eq!(
            &args[args.len() - 3..],
            ["-t", "operator@525400c0ffee", "uptime"]
        );

        // The key the platform certified is the one handed to ssh, with
        // what the platform answered.
        let certified = gateway.0.lock().unwrap().clone();
        assert_eq!(certified.len(), 1);
        assert_eq!(certified[0].1, SshRole::Admin);
        assert!(certified[0].0.starts_with("ssh-ed25519 "));
        let key = PrivateKey::from_openssh(prepared.keys.private_key()).unwrap();
        assert_eq!(key.public_key().to_openssh().unwrap(), certified[0].0);
        assert_eq!(
            prepared.keys.certificate,
            "ssh-ed25519-cert-v01@openssh.com CERT 525400C0FFEE"
        );
        assert!(prepared.keys.known_hosts.starts_with("@cert-authority "));
    }

    #[tokio::test]
    async fn a_datagram_channel_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let channel = controller(root.path(), Gateway::default()).await;
        let report = match channel.open(None, "525400C0FFEE", "datagram").await {
            Ok(_) => panic!("a datagram channel must be refused"),
            Err(report) => report,
        };
        assert!(matches!(report.current_context(), Error::Datagram(_)));
    }

    #[tokio::test]
    async fn refused_arguments_cost_no_certificate() {
        let root = tempfile::tempdir().unwrap();
        let gateway = Gateway::default();
        let channel = controller(root.path(), gateway.clone()).await;
        let report = match channel.prepare_ssh(request(&["-J", "jump"], "ssh")).await {
            Ok(_) => panic!("-J must be refused"),
            Err(report) => report,
        };
        assert!(matches!(
            report.current_context(),
            Error::RefusedArgument(_)
        ));

        let mut bypass = request(&[], "ssh");
        bypass.options = vec!["ProxyJump=elsewhere".into()];
        assert!(channel.prepare_ssh(bypass).await.is_err());
        assert!(gateway.0.lock().unwrap().is_empty());
    }

    fn scp(arguments: &[&str]) -> ScpRequest {
        ScpRequest {
            over: None,
            role: SshRole::User,
            login: None,
            options: vec![],
            arguments: arguments.iter().map(|s| s.to_string()).collect(),
            binary: "scp".into(),
            proxy_command: Arc::new(|target| {
                format!(
                    "rmra --context {} channel open {}",
                    target.context, target.device
                )
            }),
        }
    }

    #[tokio::test]
    async fn scp_pins_the_device_named_by_its_operands() {
        let root = tempfile::tempdir().unwrap();
        let channel = controller(root.path(), Gateway::default()).await;

        let prepared = channel
            .prepare_scp(scp(&["-r", "./dist", "525400C0FFEE:/data/"]))
            .await
            .unwrap();
        assert_eq!(prepared.device, "525400C0FFEE");
        let args = &prepared.command.arguments;
        assert_eq!(
            args[1],
            "ProxyCommand=rmra --context eu2 channel open 525400C0FFEE"
        );
        assert!(args.contains(&"HostKeyAlias=525400c0ffee".to_owned()));
        assert_eq!(
            &args[args.len() - 4..],
            ["-r", "--", "./dist", "operator@525400c0ffee:/data/"]
        );

        // user@ in an operand picks the account, for every remote operand.
        let prepared = channel
            .prepare_scp(scp(&[
                "root@525400C0FFEE:/etc/os-release",
                "525400c0ffee:/tmp/",
                ".",
            ]))
            .await
            .unwrap();
        let args = &prepared.command.arguments;
        assert_eq!(prepared.login, "root");
        assert_eq!(
            &args[args.len() - 3..],
            [
                "root@525400c0ffee:/etc/os-release",
                "root@525400c0ffee:/tmp/",
                "."
            ]
        );
    }

    #[tokio::test]
    async fn scp_refuses_what_one_certificate_cannot_cover() {
        let root = tempfile::tempdir().unwrap();
        let gateway = Gateway::default();
        let channel = controller(root.path(), gateway.clone()).await;
        for refused in [
            &["a", "b"][..],
            &["DEV1:/x"],
            &["DEV1:/x", "DEV2:/y"],
            &["root@DEV1:/x", "admin@DEV1:/y"],
            &["-S", "/bin/sh", "a", "DEV1:"],
        ] {
            assert!(
                channel.prepare_scp(scp(refused)).await.is_err(),
                "{refused:?}"
            );
        }
        assert!(gateway.0.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_ssh_and_removes_the_key_material_afterwards() {
        let root = tempfile::tempdir().unwrap();
        let channel = controller(root.path(), Gateway::default()).await;
        // A stand-in "ssh" that fails unless the key file it was given
        // exists, and says where it was.
        let fake = root.path().join("fake-ssh");
        let seen = root.path().join("seen");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = -i ] && test -f \"$2\" && echo \"$2\" > {} && exit 7; shift; done\nexit 1\n",
                seen.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prepared = channel
            .prepare_ssh(request(&[], fake.to_str().unwrap()))
            .await
            .unwrap();
        assert_eq!(channel.run_ssh(prepared).await.unwrap(), 7);
        let identity = std::fs::read_to_string(seen).unwrap();
        assert!(!Path::new(identity.trim_end()).exists());
    }

    #[tokio::test]
    async fn the_proxy_command_opens_the_channel_in_the_selected_context() {
        let root = tempfile::tempdir().unwrap();
        let channel = controller(root.path(), Gateway::default()).await;
        let mut ssh = request(&[], "ssh");
        ssh.proxy_command = Arc::new(|target| format!("context={}", target.context));
        let prepared = channel.prepare_ssh(ssh).await.unwrap();
        assert_eq!(prepared.command.arguments[1], "ProxyCommand=context=eu2");
    }
}
