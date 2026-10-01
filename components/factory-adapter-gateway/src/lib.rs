//! `FactoryProvisioningAdapter` against `sntns-platform`'s gateway --
//! `POST /remora/v1/factory-device`, `Authorization: X-SNTNS-API-KEY
//! <key>`, camelCase JSON with base64-encoded `bytes` fields (the stock
//! grpc-gateway marshaler, no custom options). Confirmed against a real
//! `sntns-dev` round trip on 2026-09-19 (`factoryDeviceName`,
//! `certificateReference.{id,urn,name}`, `certificate`,
//! `certificateAuthorityCertificate`, `serverCertificateAuthorityCertificate`
//! -- all base64 DER).
//!
//! `delete` (used by `provision --force`) is `DELETE
//! /remora/v1/factory-device/{deviceName}`, same auth header; a 404 means
//! there was nothing to delete and is treated as success.

mod service;

pub use service::GatewayAdapterImpl;
