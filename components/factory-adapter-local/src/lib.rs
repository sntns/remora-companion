//! The factory vertical's two local adapters: the device keypair + CSR
//! (`DeviceKeyAdapter`, over rcgen) and `remora-factory.yaml`
//! (`CredentialWriterAdapter`). One crate because both are the same local
//! backend for one vertical; the network half is `remora-factory-adapter-grpc`.

mod service;
mod yaml;

pub use service::{CredentialWriterAdapterImpl, DeviceKeyAdapterImpl};
