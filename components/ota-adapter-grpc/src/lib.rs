mod service;
#[cfg(feature = "test-gateway")]
pub mod test_gateway;

pub use service::OtaGatewayAdapterImpl;
