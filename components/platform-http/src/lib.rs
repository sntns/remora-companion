//! The wire shared by every adapter that talks to sntns-platform's public
//! gateways (iam, remora, remora channel): their REST API -- grpc-gateway's
//! JSON over HTTP/1.1, plus the hand-written artifact routes -- and their
//! WebSockets, behind one [`Gateway`] that applies TLS and the platform's
//! credentials, and turns every refusal into a [`Status`].
//!
//! A utility crate rather than a vertical: it holds no business decision,
//! only the wire. It knows the context vertical's model (an inert value
//! type, see CLAUDE.md) so that every adapter builds its [`Connection`] the
//! same way, with [`Connection::for_resolved`].

mod connection;
mod error;
mod gateway;
pub mod model;
#[cfg(feature = "test-gateway")]
pub mod test_gateway;

pub use connection::{Connection, Credentials, Tls};
pub use error::{status_summary, Code, Error, Result, Status};
pub use gateway::{Gateway, WebSocket};
pub use reqwest::{Body, Method, Response, StatusCode};
pub use tokio_tungstenite::tungstenite::{protocol::CloseFrame, Message};
pub use url::Url;
