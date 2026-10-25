//! `FactoryProvisioningAdapter` over the remora gateway's factory devices
//! (`POST /remora/v1/factory-device`, then its assignment), authenticated
//! like every other call as the selected context: its login, and the role
//! it acts as. The manufacturing account is the caller's own -- the request
//! names none.

mod service;

pub use service::FactoryGatewayAdapterImpl;
