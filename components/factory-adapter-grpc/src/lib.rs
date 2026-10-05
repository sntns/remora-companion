//! `FactoryProvisioningAdapter` over the remora gateway's
//! `DeviceService.CreateFactoryDevice`, authenticated like every other call
//! as the selected context: its login, and the role it acts as. The
//! manufacturing account is the caller's own -- the request names none.

mod service;

pub use service::FactoryGatewayAdapterImpl;
