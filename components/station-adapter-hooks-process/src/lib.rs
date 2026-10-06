//! `HookRunnerAdapter` as child processes: each event's `<event>.d/`
//! scripts, run-parts style -- what lets a station print labels or feed a
//! registry without the station knowing how.

mod service;

pub use service::ProcessHookRunnerImpl;
