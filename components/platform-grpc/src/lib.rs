//! gRPC plumbing shared by every adapter that talks to sntns-platform's
//! public gateways: the generated clients (and servers, for tests), and one
//! [`connect`] that applies TLS and the platform's per-RPC credentials.
//!
//! A utility crate rather than a vertical: it holds no business decision,
//! only the wire. It knows the context vertical's model (an inert value
//! type, see CLAUDE.md) so that every adapter builds its [`Connection`] the
//! same way, with [`Connection::for_context`].

mod connection;
mod error;

pub use connection::{connect, AuthInterceptor, Connection, Credentials, GatewayChannel, Tls};
pub use error::{status_summary, Error, Result};

pub mod sntns {
    pub mod service {
        pub mod v1 {
            tonic::include_proto!("sntns.service.v1");
        }
        pub mod remorachannel {
            pub mod v1 {
                tonic::include_proto!("sntns.service.remorachannel.v1");
            }
        }
        pub mod iam {
            pub mod v1 {
                tonic::include_proto!("sntns.service.iam.v1");
            }
        }
    }
}
