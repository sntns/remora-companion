//! Lines for when stderr can't take decorations: shared with a program
//! that has the terminal in raw mode, or a debug trace meant to be read
//! (and pasted) as plain text.

use console::style;

/// An error on one line of a terminal another program has put in raw mode
/// -- as a ssh ProxyCommand, whose stderr ssh passes through: decorated,
/// multi-line output would stair-step across the screen. Explicit carriage
/// returns read right either way. `program` says who is speaking, since
/// the line lands in someone else's session.
pub fn raw_error(program: &str, message: impl std::fmt::Display) {
    eprint!("\r\n{program}: {message}\r\n");
}

/// One `debug:` detail line (`--verbose`): a label and its value, labels
/// aligned, never decorated beyond a dim prefix so it pastes as text.
pub fn debug(label: &str, value: impl std::fmt::Display) {
    eprintln!("{} {label:<9} {value}", style("debug:").dim());
}
