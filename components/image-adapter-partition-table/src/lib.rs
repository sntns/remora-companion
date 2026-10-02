//! GPT/MBR partition table reading, and ext4/vfat signature detection —
//! never shelling out to any external tool.

mod gpt;
mod mbr;
mod service;

pub use service::PartitionTableAdapterImpl;
