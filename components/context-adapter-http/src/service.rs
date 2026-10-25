use error_stack::{Report, ResultExt};
use remora_context::{
    adapter::platform::{Error, PlatformSessionAdapter, Result},
    model::{Context, Credentials, Principal},
};
use remora_platform_http::{model, status_summary, Code, Connection, Gateway, Status};

/// Asks the IAM gateway who the credentials are: the current user (which
/// any authenticated principal may read), then the current account for
/// its name, best-effort -- a principal without that permission is still
/// logged in.
pub struct PlatformSessionAdapterImpl;

#[async_trait::async_trait]
impl PlatformSessionAdapter for PlatformSessionAdapterImpl {
    async fn whoami(&self, context: &Context, credentials: &Credentials) -> Result<Principal> {
        // As the login itself, never a role: the current user describes
        // the principal, and an assumed role's principal is no user.
        let gateway = Gateway::new(&Connection::for_context(context, Some(credentials), None))
            .await
            .change_context(Error::Unreachable)?;

        let user = gateway
            .get::<model::GetCurrentUserResponse>(gateway.url(["iam", "v1", "current-user"]))
            .await
            .map_err(classify)?
            .user_descriptor
            .and_then(|descriptor| descriptor.resource)
            .unwrap_or_default();

        let account_name = gateway
            .get::<model::GetCurrentAccountResponse>(gateway.url(["iam", "v1", "current-account"]))
            .await
            .ok()
            .and_then(|response| response.account_descriptor)
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
        let gateway = Gateway::new(&Connection::for_context(
            context,
            Some(credentials),
            Some(role),
        ))
        .await
        .change_context(Error::Unreachable)?;
        // Any authenticated call assumes the role first; the current
        // account also says where it lands -- the role's own tenant.
        match gateway
            .get::<model::GetCurrentAccountResponse>(gateway.url(["iam", "v1", "current-account"]))
            .await
        {
            Ok(response) => Ok(response
                .account_descriptor
                .and_then(|descriptor| descriptor.resource)
                .map(|resource| resource.name)
                .filter(|name| !name.is_empty())),
            Err(status) => match status.code {
                // The assumption itself refused ("role permission denied",
                // or the role doesn't exist), as opposed to a role that
                // assumed fine but may not read its account.
                Code::PermissionDenied | Code::Unauthenticated | Code::NotFound
                    if status.message.contains("role") =>
                {
                    Err(Report::new(Error::RoleRefused).attach(status_summary(&status)))
                }
                Code::PermissionDenied => Ok(None),
                _ => Err(classify(status)),
            },
        }
    }
}

fn classify(status: Status) -> Report<Error> {
    let error = match status.code {
        Code::Unauthenticated | Code::PermissionDenied => Error::Unauthenticated,
        Code::Unavailable => Error::Unreachable,
        _ => Error::Call,
    };
    Report::new(error).attach(status_summary(&status))
}

#[cfg(test)]
// The fake gateway's handlers answer `Result<_, Response>`, axum's own
// idiom for a refusal.
#[allow(clippy::result_large_err)]
mod tests {
    use remora_context::model::{Endpoint, Secret, Tls};
    use remora_platform_http::{
        model::ResourceReference,
        test_gateway::{
            axum::{http::HeaderMap, response::Response, routing::get, Json, Router},
            header, rpc_error, serve_router,
        },
    };

    use super::*;

    /// An in-process IAM gateway that knows one access key.
    fn authenticated(headers: &HeaderMap) -> std::result::Result<(), Response> {
        match header(headers, "authentication-token") {
            Some("good") => Ok(()),
            _ => Err(rpc_error(Code::Unauthenticated, "invalid access key")),
        }
    }

    async fn current_user(
        headers: HeaderMap,
    ) -> std::result::Result<Json<model::GetCurrentUserResponse>, Response> {
        authenticated(&headers)?;
        // As the platform does: an assumed role's principal is no user.
        if header(&headers, "assume-role").is_some() {
            return Err(rpc_error(Code::NotFound, "user not found"));
        }
        Ok(Json(model::GetCurrentUserResponse {
            user_descriptor: Some(model::UserDescriptor {
                resource: Some(ResourceReference {
                    id: "1".into(),
                    urn: "urn:sntns:iam:eu2:acme:user:ada".into(),
                    name: "ada".into(),
                }),
                ..Default::default()
            }),
        }))
    }

    /// Assuming a role lands in its tenant's account; the platform refuses
    /// an assumption with "role permission denied", and a role that may not
    /// read its account with a plain permission error.
    async fn current_account(
        headers: HeaderMap,
    ) -> std::result::Result<Json<model::GetCurrentAccountResponse>, Response> {
        authenticated(&headers)?;
        let account = match header(&headers, "assume-role").unwrap_or_default() {
            "" => "acme",
            "urn:sntns:iam:eu2:other:role:ops" => "other",
            "urn:sntns:iam:eu2:other:role:limited" => {
                return Err(rpc_error(
                    Code::PermissionDenied,
                    "account permission denied",
                ))
            }
            _ => return Err(rpc_error(Code::PermissionDenied, "role permission denied")),
        };
        Ok(Json(model::GetCurrentAccountResponse {
            account_descriptor: Some(model::AccountDescriptor {
                resource: Some(ResourceReference {
                    name: account.into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        }))
    }

    async fn serve() -> String {
        serve_router(
            Router::new()
                .route("/iam/v1/current-user", get(current_user))
                .route("/iam/v1/current-account", get(current_account)),
        )
        .await
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
            login: None,
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
    async fn an_unreachable_gateway_is_said_so() {
        // Nothing listens on the discard port.
        let report = PlatformSessionAdapterImpl
            .whoami(&context("127.0.0.1:9".into()), &token("good"))
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Unreachable));
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
