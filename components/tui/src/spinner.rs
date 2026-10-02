use std::fmt::Display;

/// A step in progress: an animated spinner on a terminal, a plain line
/// otherwise. Always end it with [`Spinner::done`] or [`Spinner::fail`];
/// dropping it unfinished clears the line.
pub struct Spinner(Option<cliclack::ProgressBar>);

impl Spinner {
    pub fn start(message: impl Display) -> Self {
        if crate::decorated() {
            let bar = cliclack::spinner();
            bar.start(message);
            Self(Some(bar))
        } else {
            eprintln!("{message}...");
            Self(None)
        }
    }

    pub fn set_message(&self, message: impl Display) {
        if let Some(bar) = &self.0 {
            bar.set_message(message);
        }
    }

    pub fn done(mut self, message: impl Display) {
        match self.0.take() {
            Some(bar) => bar.stop(message),
            None => eprintln!("{message}"),
        }
    }

    pub fn fail(mut self, message: impl Display) {
        match self.0.take() {
            Some(bar) => bar.error(message),
            None => eprintln!("{message}"),
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if let Some(bar) = self.0.take() {
            bar.clear();
        }
    }
}
