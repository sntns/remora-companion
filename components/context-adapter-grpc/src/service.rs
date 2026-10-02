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
        let channel = connect(&Connection::for_context(context, Some(credentials)))
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

    async fn serve() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(iam::user_service_server::UserServiceServer::new(FakeIam))
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
        }
    }

    fn token(token: &str) -> Credentials {
        Credentials {
            secret: Secret::AccessKey {
                token: token.into(),
            },
            assume_role: None,
        }
    }

    #[tokio::test]
    async fn whoami_names_the_user_without_an_account_service() {
        let context = context(serve().await);
        let principal = PlatformSessionAdapterImpl
            .whoami(&context, &token("good"))
            .await
            .unwrap();
        assert_eq!(principal.user_name, "ada");
        assert_eq!(principal.user_urn, "urn:sntns:iam:eu2:acme:user:ada");
        // The fake serves no AccountService: Unimplemented is not fatal.
        assert_eq!(principal.account_name, None);
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
}
