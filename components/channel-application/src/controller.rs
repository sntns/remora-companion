use std::path::Path;

use error_stack::{Report, ResultExt};
use remora_channel::{
    adapter::{
        gateway::{Channel, ChannelGatewayAdapterService},
        ssh::SshClientAdapterService,
    },
    application::{ChannelServiceInterface, Error, Result},
    model::{
        PreparedSsh, ProxyCommandBuilder, ProxyTarget, ScpRequest, SshCertificate, SshCommand,
        SshRequest, SshRole,
    },
};
use remora_context::{
    application::ContextService,
    model::{ContextOverride, ResolvedContext},
};
use ssh_key::{rand_core::OsRng, Algorithm, LineEnding, PrivateKey};

use crate::arguments::{self, Operand};

/// The channel vertical's use case: the gateway port for channels and
/// certificates, the ssh-client port to run ssh, and the context vertical's
/// application port to know which platform, as whom.
pub struct ChannelControllerImpl {
    contexts: ContextService,
    gateway: ChannelGatewayAdapterService,
    ssh: SshClientAdapterService,
}

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
        }
    }

    /// What every ssh-family session needs: a throwaway ed25519 key in a
    /// private directory, certified for `role` on `device`, and the options
    /// that pin ssh to the device's channel, host authority and that
    /// certificate -- followed by the caller's own `-o` options, which come
    /// after so ssh (first value wins) never lets them loosen the pinning.
    async fn certify(
        &self,
        over: Option<&ContextOverride>,
        device: &str,
        role: SshRole,
        proxy_command: &ProxyCommandBuilder,
        options: Vec<String>,
    ) -> Result<Session> {
        let context = self.resolve(over).await?;

        // 0700 and removed when the PreparedSsh drops: it holds a private
        // key, even if one that is worthless in a few minutes.
        let mut builder = tempfile::Builder::new();
        builder.prefix("rmra-ssh-");
        // Explicitly: tempfile only applies the umask, which is 0775 for
        // many users.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let workdir = builder.tempdir().change_context(Error::Workdir)?;

        let key =
            PrivateKey::random(&mut OsRng, Algorithm::Ed25519).change_context(Error::Keygen)?;
        let public_key = key
            .public_key()
            .to_openssh()
            .change_context(Error::Keygen)?;
        let identity = workdir.path().join("id_ed25519");
        let private_pem = key
            .to_openssh(LineEnding::LF)
            .change_context(Error::Keygen)?;
        write_private(&identity, private_pem.as_bytes())?;

        let certified = self
            .gateway
            .sign_ssh_certificate(&context, device, &public_key, role)
            .await
            .change_context_lazy(|| Error::Certify(device.to_owned()))?;
        let certificate = workdir.path().join("id_ed25519-cert.pub");
        write_private(
            &certificate,
            format!("{}\n", certified.certificate.trim_end()).as_bytes(),
        )?;
        let known_hosts = workdir.path().join("known_hosts");
        write_private(
            &known_hosts,
            format!("{}\n", certified.known_hosts.trim_end()).as_bytes(),
        )?;

        let proxy_command = proxy_command(&ProxyTarget {
            context: &context.context.name,
            role: context.role.as_ref().map(|role| role.urn.as_str()),
            device,
        });
        let mut pinning = vec![
            "-o".to_owned(),
            format!("ProxyCommand={proxy_command}"),
            "-o".to_owned(),
            "IdentitiesOnly=yes".to_owned(),
            "-i".to_owned(),
            identity.display().to_string(),
            "-o".to_owned(),
            format!("CertificateFile={}", certificate.display()),
            "-o".to_owned(),
            format!("UserKnownHostsFile={}", known_hosts.display()),
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
        Ok(Session {
            context: context.context.name,
            certified,
            pinning,
            workdir,
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
        self.gateway
            .open(&context, device, profile)
            .await
            .change_context_lazy(|| Error::Open(device.to_owned()))
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

        Ok(PreparedSsh::new(
            session.context,
            request.device,
            login,
            session.certified.host_key_alias,
            request.role,
            SshCommand {
                program: request.binary,
                arguments,
            },
            session.workdir,
        ))
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

        Ok(PreparedSsh::new(
            session.context,
            device,
            login,
            alias,
            request.role,
            SshCommand {
                program: request.binary,
                arguments,
            },
            session.workdir,
        ))
    }

    async fn run_ssh(&self, prepared: PreparedSsh) -> Result<i32> {
        let code = self
            .ssh
            .run(&prepared.command)
            .await
            .change_context(Error::Ssh)?;
        drop(prepared);
        Ok(code)
    }
}

/// A certified session before its client-specific arguments.
struct Session {
    context: String,
    certified: SshCertificate,
    pinning: Vec<String>,
    workdir: tempfile::TempDir,
}

fn check_options(options: &[String]) -> Result<()> {
    for option in options {
        arguments::check_option(option)
            .map_err(|reason| Report::new(Error::RefusedArgument(reason)))?;
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .change_context(Error::Workdir)
        .attach_with(|| path.display().to_string())
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
    use std::sync::{Arc, Mutex};

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
        model::{
            Context, ContextOverride, Credentials, Endpoint, Principal, RoleOverride, Secret,
            Selection, Tls,
        },
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
        async fn open(&self, _: &ResolvedContext, _: &str, _: &str) -> gateway::Result<Channel> {
            Err(Report::new(gateway::Error::NotConnected))
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

        // The key the platform certified is the one written for ssh.
        let certified = gateway.0.lock().unwrap().clone();
        assert_eq!(certified.len(), 1);
        assert_eq!(certified[0].1, SshRole::Admin);
        assert!(certified[0].0.starts_with("ssh-ed25519 "));
        let identity = prepared.workdir().join("id_ed25519");
        let key = PrivateKey::read_openssh_file(&identity).unwrap();
        assert_eq!(key.public_key().to_openssh().unwrap(), certified[0].0);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(prepared.workdir()), 0o700);
            assert_eq!(mode(&identity), 0o600);
        }
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
        // A stand-in "ssh" that fails unless the key file it was given exists.
        let fake = root.path().join("fake-ssh");
        std::fs::write(
            &fake,
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = -i ] && test -f \"$2\" && exit 7; shift; done\nexit 1\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prepared = channel
            .prepare_ssh(request(&[], fake.to_str().unwrap()))
            .await
            .unwrap();
        let workdir = prepared.workdir().to_path_buf();
        assert_eq!(channel.run_ssh(prepared).await.unwrap(), 7);
        assert!(!workdir.exists());
    }

    #[tokio::test]
    async fn the_proxy_command_opens_the_channel_as_the_certified_role() {
        let root = tempfile::tempdir().unwrap();
        let channel = controller(root.path(), Gateway::default()).await;
        let mut ssh = request(&[], "ssh");
        ssh.over = Some(ContextOverride {
            name: None,
            source: Selection::Flag,
            role: RoleOverride::Assume("urn:sntns:iam:eu2:other:role:ops".into()),
        });
        ssh.proxy_command = Arc::new(|target| format!("role={:?}", target.role));
        let prepared = channel.prepare_ssh(ssh).await.unwrap();
        assert_eq!(
            prepared.command.arguments[1],
            "ProxyCommand=role=Some(\"urn:sntns:iam:eu2:other:role:ops\")"
        );

        let mut ssh = request(&[], "ssh");
        ssh.proxy_command = Arc::new(|target| format!("role={:?}", target.role));
        let prepared = channel.prepare_ssh(ssh).await.unwrap();
        assert_eq!(prepared.command.arguments[1], "ProxyCommand=role=None");
    }
}
