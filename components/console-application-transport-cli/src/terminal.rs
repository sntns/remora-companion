use std::io::Write;

use crossterm::{
    cursor, event,
    terminal::{self, ClearType},
    QueueableCommand,
};
use error_stack::{Report, ResultExt};

use crate::error::{Error, Result};

/// The operator's terminal, raw and on its alternate screen while the
/// console runs; given back as it was however the console ends.
pub(crate) struct Terminal {
    out: std::io::Stdout,
    /// The device's screen as last drawn, for drawing only what changed.
    drawn: Option<vt100::Screen>,
}

impl Terminal {
    pub(crate) fn enter() -> Result<Self> {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err(Report::new(Error::NotATerminal));
        }
        terminal::enable_raw_mode().change_context(Error::Terminal)?;
        let mut out = std::io::stdout();
        let entered = out
            .queue(terminal::EnterAlternateScreen)
            .and_then(|out| out.queue(event::EnableBracketedPaste))
            .and_then(|out| out.queue(terminal::Clear(ClearType::All)))
            .and_then(|out| out.flush());
        let terminal = Self { out, drawn: None };
        entered.change_context(Error::Terminal)?;
        Ok(terminal)
    }

    /// Rows and columns.
    pub(crate) fn size() -> Result<(u16, u16)> {
        let (cols, rows) = terminal::size().change_context(Error::Terminal)?;
        Ok((rows, cols))
    }

    /// Everything is drawn anew next time (after a resize).
    pub(crate) fn invalidate(&mut self) {
        self.drawn = None;
    }

    /// Draws `screen` -- only what changed since the last time -- and the
    /// status line on the row below it.
    pub(crate) fn draw(&mut self, screen: &vt100::Screen, status: &str) -> Result<()> {
        let (rows, cols) = screen.size();
        let mut frame = match &self.drawn {
            Some(drawn) => screen.contents_diff(drawn),
            None => {
                let mut all = b"\x1b[H\x1b[2J".to_vec();
                all.extend(screen.contents_formatted());
                all
            }
        };
        // The status line, reverse video, then the device's own drawing
        // state back: its attributes, cursor position and visibility.
        let status: String = status.chars().take(cols.into()).collect();
        frame.extend(
            format!(
                "\x1b[{};1H\x1b[0;7m{status:<width$}\x1b[0m",
                rows + 1,
                width = usize::from(cols)
            )
            .into_bytes(),
        );
        frame.extend(screen.attributes_formatted());
        frame.extend(screen.cursor_state_formatted());
        self.out.write_all(&frame).change_context(Error::Terminal)?;
        self.out.flush().change_context(Error::Terminal)?;
        self.drawn = Some(screen.clone());
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self
            .out
            .queue(event::DisableBracketedPaste)
            .and_then(|out| out.queue(cursor::Show))
            .and_then(|out| out.queue(terminal::LeaveAlternateScreen))
            .and_then(|out| out.flush());
        let _ = terminal::disable_raw_mode();
    }
}
