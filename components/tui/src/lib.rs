//! Terminal presentation shared by the CLI transport crates, so every
//! command looks the same: clack-style lines, spinners and prompts, tables,
//! and errors.
//!
//! Everything interactive or decorative goes to **stderr**; stdout carries
//! only a command's actual output (a table, JSON, a tunnel's bytes), so it
//! stays pipeable. When stderr is not a terminal the decorations degrade to
//! plain lines and prompts refuse rather than hang.
//!
//! Not a port: how a line is drawn is not a business decision any test
//! needs to substitute (same reasoning as `remora-scratch`).

mod report;
mod spinner;
mod table;

pub use report::render_report;
pub use spinner::Spinner;
pub use table::Table;

pub use cliclack::{confirm, input, password, select};
use console::style;

/// Whether a human is at the terminal: both stdin (to answer prompts) and
/// stderr (to see them) are terminals.
pub fn interactive() -> bool {
    decorated() && console::user_attended()
}

/// Whether stderr is a terminal that can draw, i.e. whether decorations are
/// worth drawing. `is_term` rather than `is_attended`: it also says no for
/// an unset or `dumb` `TERM`, which is exactly when the spinners and bars
/// (indicatif, under cliclack) hide themselves -- so everything falls back
/// to plain lines together instead of the bars vanishing silently.
pub fn decorated() -> bool {
    console::Term::stderr().is_term()
}

/// A header line opening a multi-step command.
pub fn intro(title: impl std::fmt::Display) {
    if decorated() {
        let _ = cliclack::intro(style(format!(" {title} ")).on_cyan().black());
    }
}

/// The closing line of a multi-step command.
pub fn outro(message: impl std::fmt::Display) {
    if decorated() {
        let _ = cliclack::outro(message);
    } else {
        eprintln!("{message}");
    }
}

pub fn success(message: impl std::fmt::Display) {
    if decorated() {
        let _ = cliclack::log::success(message);
    } else {
        eprintln!("{message}");
    }
}

pub fn info(message: impl std::fmt::Display) {
    if decorated() {
        let _ = cliclack::log::info(message);
    } else {
        eprintln!("{message}");
    }
}

pub fn step(message: impl std::fmt::Display) {
    if decorated() {
        let _ = cliclack::log::step(message);
    } else {
        eprintln!("{message}");
    }
}

pub fn warning(message: impl std::fmt::Display) {
    if decorated() {
        let _ = cliclack::log::warning(message);
    } else {
        eprintln!("warning: {message}");
    }
}

/// A titled block of detail lines, e.g. a context's settings.
pub fn note(title: impl std::fmt::Display, body: impl std::fmt::Display) {
    if decorated() {
        let _ = cliclack::note(title, body);
    } else {
        eprintln!("{title}\n{body}");
    }
}

/// Emphasis for a name inside a message (a context, a device, a user).
pub fn accent(text: impl std::fmt::Display) -> String {
    style(text).cyan().bold().to_string()
}

/// De-emphasis for secondary detail inside a message.
pub fn dim(text: impl std::fmt::Display) -> String {
    style(text).dim().to_string()
}
