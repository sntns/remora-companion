//! Reads whichever partition table (`mbrman` for MBR, `gptman` for GPT) is
//! actually on disk, auto-detected: GPT is tried first (it starts with a
//! protective MBR, so a plain MBR reader would otherwise misread it as one
//! giant 0xEE partition), falling back to plain MBR.

mod gpt;
mod mbr;

pub mod error;
pub mod port;
pub mod service;

pub use error::Error;
pub use port::{PartitionTableAdapter, PartitionTableReader};
pub use service::{detect_fs_kind, read};
