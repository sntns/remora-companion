//! The device's side of the provisioning station: what a hub booting a
//! cloned image does to get its identity (`remora-station` is the other
//! side). Here it backs `station simulate`, a hub played on a laptop.

pub mod adapter;
pub mod application;
pub mod model;
