mod error;
mod model;
mod service;
#[cfg(feature = "walkdir")]
mod walkdir_backed;

pub use error::{Error, Result};
pub use model::{WalkEntry, WalkEntryKind, WalkEntryMetadata};
pub use service::{FsWalkAdapter, FsWalkAdapterService};
#[cfg(feature = "walkdir")]
pub use walkdir_backed::FsWalkAdapterImpl;
