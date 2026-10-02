//! `FactoryProvisioningAdapter` against `sntns-platform`'s gateway --
//! `POST /remora/v1/factory-device`, `Authorization: X-SNTNS-API-KEY
//! <key>`, camelCase JSON with base64-encoded `bytes` fields (the stock
//! grpc-gateway marshaler, no custom options). Confirmed against a real
//! `sntns-dev` round trip on 2026-09-19 (`factoryDeviceName`,
//! `certificateReference.{id,urn,name}`, `certificate`,
//! `certificateAuthorityCertificate`, `serverCertificateAuthorityCertificate`
//! -- all base64 DER).
//!
//! The request names the serial one of two ways: `serialNumberPolicyName`
//! (the platform allocates a fresh serial, returned as `serialNumber`) or
//! `deviceName` (an externally chosen serial). An existing `deviceName` is
//! refused with 409 unless `force: true`, which re-signs it and revokes the
//! old IDevID server-side -- there is no separate delete call any more.

mod service;

pub use service::GatewayAdapterImpl;
