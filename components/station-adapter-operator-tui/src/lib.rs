//! `OperatorAdapter` on the terminal, through `remora-tui`: what happens
//! and which hub to label on stderr, each issued serial alone on stdout,
//! and stdin read a line at a time -- a barcode scanner types the label
//! then Enter, and a command is its letter then Enter.

mod service;

pub use service::{parse_input, TuiOperatorImpl};
