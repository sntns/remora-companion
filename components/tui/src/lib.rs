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

mod progress;
mod report;
mod spinner;
mod table;
mod time;

pub use progress::{Progress, Watch, WatchLine};
pub use report::render_report;
pub use spinner::Spinner;
pub use table::Table;
pub use time::{ago, timestamp};

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

/// What a status means, for its color: the same word reads the same
/// everywhere (a table cell, a watch line, a summary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Done, as asked (green).
    Good,
    /// Failed or refused (red).
    Bad,
    /// In progress (cyan).
    Active,
    /// Waiting, or a draft (dim).
    Idle,
    /// Being undone, or needs attention (yellow).
    Warn,
}

/// `text` in its tone's color.
pub fn toned(text: impl std::fmt::Display, tone: Tone) -> String {
    let styled = style(text.to_string());
    match tone {
        Tone::Good => styled.green().bold(),
        Tone::Bad => styled.red().bold(),
        Tone::Active => styled.cyan(),
        Tone::Idle => styled.dim(),
        Tone::Warn => styled.yellow(),
    }
    .to_string()
}

/// A byte count for a human: 1.5 GiB, 340 KiB.
pub fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
