use error_stack::{Report, ResultExt};
use remora_context::{
    adapter::platform::{Error, PlatformSessionAdapter, Result},
    model::{Context, Credentials, Principal},
};
use remora_platform_grpc::{connect, sntns::service::iam::v1 as iam, status_summary, Connection};

/// Asks the IAM gateway who the credentials are: `GetCurrentUser` (which
/// any authenticated principal may call), then `GetCurrentAccount` for the
/// account's name, best-effort -- a principal without that permission is
/// still logged in.
pub struct PlatformSessionAdapterImpl;

#[async_trait::async_trait]
impl PlatformSessionAdapter for PlatformSessionAdapterImpl {
    async fn whoami(&self, context: &Context, credentials: &Credentials) -> Result<Principal> {
        // As the login itself, never a role: GetCurrentUser describes the
        // principal, and an assumed role's principal is no user.
        let channel = connect(&Connection::for_context(context, Some(credentials), None))
            .await
            .change_context(Error::Unreachable)?;

        let user = iam::user_service_client::UserServiceClient::new(channel.clone())
            .get_current_user(iam::UserServiceGetCurrentUserRequest {})
            .await
            .map_err(classify)?
            .into_inner()
            .user_descriptor
            .and_then(|descriptor| descriptor.resource)
            .unwrap_or_default();

        let account_name = iam::account_service_client::AccountServiceClient::new(channel)
            .get_current_account(iam::AccountServiceGetCurrentAccountRequest {})
            .await
            .ok()
            .and_then(|response| response.into_inner().account_descriptor)
            .and_then(|descriptor| descriptor.resource)
            .map(|resource| resource.name)
            .filter(|name| !name.is_empty());

        Ok(Principal {
            user_urn: user.urn,
            user_name: user.name,
            account_name,
        })
    }

    async fn acting_account(
        &self,
        context: &Context,
        credentials: &Credentials,
        role: &str,
    ) -> Result<Option<String>> {
        let channel = connect(&Connection::for_context(
            context,
            Some(credentials),
            Some(role),
        ))
        .await
        .change_context(Error::Unreachable)?;
        // Any authenticated call assumes the role first; GetCurrentAccount
        // also says where it lands -- the role's own tenant.
        match iam::account_service_client::AccountServiceClient::new(channel)
            .get_current_account(iam::AccountServiceGetCurrentAccountRequest {})
            .await
        {
            Ok(response) => Ok(response
                .into_inner()
                .account_descriptor
                .and_then(|descriptor| descriptor.resource)
                .map(|resource| resource.name)
                .filter(|name| !name.is_empty())),
            Err(status) => match status.code() {
                // The assumption itself refused ("role permission denied",
                // or the role doesn't exist), as opposed to a role that
                // assumed fine but may not read its account.
                tonic::Code::PermissionDenied
                | tonic::Code::Unauthenticated
                | tonic::Code::NotFound
                    if status.message().contains("role") =>
                {
                    Err(Report::new(Error::RoleRefused).attach(status_summary(&status)))
                }
                tonic::Code::PermissionDenied => Ok(None),
                _ => Err(classify(status)),
            },
        }
    }
}

fn classify(status: tonic::Status) -> Report<Error> {
    let error = match status.code() {
        tonic::Code::Unauthenticated | tonic::Code::PermissionDenied => Error::Unauthenticated,
        tonic::Code::Unavailable => Error::Unreachable,
        _ => Error::Call,
    };
    Report::new(error).attach(status_summary(&status))
}

#[cfg(test)]
mod tests {
    use remora_context::model::{Endpoint, Secret, Tls};
    use remora_platform_grpc::sntns::service::v1::ResourceReference;
    use tonic::{Request, Response, Status};

    use super::*;

    /// An in-process IAM gateway that knows one access key.
    struct FakeIam;

    #[tonic::async_trait]
    impl iam::user_service_server::UserService for FakeIam {
        async fn get_current_user(
            &self,
            request: Request<iam::UserServiceGetCurrentUserRequest>,
        ) -> std::result::Result<Response<iam::UserServiceGetCurrentUserResponse>, Status> {
            match request.metadata().get("authentication-token") {
                Some(token) if token == "good" => {}
                _ => return Err(Status::unauthenticated("invalid access key")),
            }
            // As the platform does: an assumed role's principal is no user.
            if request.metadata().get("assume-role").is_some() {
                return Err(Status::not_found("user not found"));
            }
            Ok(Response::new(iam::UserServiceGetCurrentUserResponse {
                user_descriptor: Some(iam::UserDescriptor {
                    resource: Some(ResourceReference {
                        id: "1".into(),
                        urn: "urn:sntns:iam:eu2:acme:user:ada".into(),
                        name: "ada".into(),
                    }),
                    ..Default::default()
                }),
            }))
        }
    }

    /// Assuming a role lands in its tenant's account; the platform refuses
    /// an assumption with "role permission denied", and a role that may not
    /// read its account with a plain permission error.
    #[tonic::async_trait]
    impl iam::account_service_server::AccountService for FakeIam {
        async fn get_current_account(
            &self,
            request: Request<iam::AccountServiceGetCurrentAccountRequest>,
        ) -> std::result::Result<Response<iam::AccountServiceGetCurrentAccountResponse>, Status>
        {
            let role = request
                .metadata()
                .get("assume-role")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let account = match role.as_str() {
                "" => "acme",
                "urn:sntns:iam:eu2:other:role:ops" => "other",
                "urn:sntns:iam:eu2:other:role:limited" => {
                    return Err(Status::permission_denied("account permission denied"))
                }
                _ => return Err(Status::permission_denied("role permission denied")),
            };
            Ok(Response::new(
                iam::AccountServiceGetCurrentAccountResponse {
                    account_descriptor: Some(iam::AccountDescriptor {
                        resource: Some(ResourceReference {
                            name: account.into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                },
            ))
        }
    }

    async fn serve() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(iam::user_service_server::UserServiceServer::new(FakeIam))
                .add_service(iam::account_service_server::AccountServiceServer::new(
                    FakeIam,
                ))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );
        address
    }

    fn context(address: String) -> Context {
        Context {
            name: "local".into(),
            description: None,
            endpoint: Endpoint {
                address,
                tls: Tls {
                    disabled: true,
                    ..Tls::default()
                },
            },
            roles: Default::default(),
            assumed_role: None,
        }
    }

    fn token(token: &str) -> Credentials {
        Credentials {
            secret: Secret::AccessKey {
                token: token.into(),
            },
        }
    }

    #[tokio::test]
    async fn whoami_names_the_user_and_account() {
        let context = context(serve().await);
        let principal = PlatformSessionAdapterImpl
            .whoami(&context, &token("good"))
            .await
            .unwrap();
        assert_eq!(principal.user_name, "ada");
        assert_eq!(principal.user_urn, "urn:sntns:iam:eu2:acme:user:ada");
        assert_eq!(principal.account_name.as_deref(), Some("acme"));
    }

    #[tokio::test]
    async fn whoami_reports_refused_credentials() {
        let context = context(serve().await);
        let report = PlatformSessionAdapterImpl
            .whoami(&context, &token("bad"))
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Unauthenticated));
        assert!(format!("{report:?}").contains("invalid access key"));
    }

    #[tokio::test]
    async fn assuming_a_role_lands_in_its_tenant_or_is_refused() {
        let context = context(serve().await);
        let login = token("good");
        let acting = |role: &'static str| {
            let context = context.clone();
            let login = login.clone();
            async move {
                PlatformSessionAdapterImpl
                    .acting_account(&context, &login, role)
                    .await
            }
        };
        assert_eq!(
            acting("urn:sntns:iam:eu2:other:role:ops")
                .await
                .unwrap()
                .as_deref(),
            Some("other")
        );
        assert_eq!(
            acting("urn:sntns:iam:eu2:other:role:limited")
                .await
                .unwrap(),
            None
        );
        let report = acting("urn:sntns:iam:eu2:other:role:forbidden")
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::RoleRefused));
        assert!(format!("{report:?}").contains("role permission denied"));

        // whoami stays the login's own, never the role's.
        let principal = PlatformSessionAdapterImpl
            .whoami(&context, &login)
            .await
            .unwrap();
        assert_eq!(principal.account_name.as_deref(), Some("acme"));
    }
}
