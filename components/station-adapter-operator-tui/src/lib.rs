//! `OperatorAdapter` on the terminal, through `remora-tui`: what happens
//! and which hub to label on stderr, each issued serial alone on stdout.
//! What the operator types is read by `station serve` itself.

mod service;

pub use service::TuiOperatorImpl;
