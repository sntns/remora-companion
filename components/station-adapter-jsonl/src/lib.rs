//! `JournalAdapter` as JSON Lines: the station's memory across restarts
//! and its production register, in a format any tool can read.

mod service;

pub use service::JsonlJournalImpl;
