mod error;
mod service;

pub use error::{Error, Result};
pub use service::{JournalAdapter, JournalAdapterService, JournalEntry, JournalEvent};
