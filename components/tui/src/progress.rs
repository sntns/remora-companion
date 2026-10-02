use std::{collections::HashMap, fmt::Display};

/// A byte-counting progress bar for one transfer, or plain lines every
/// tenth when stderr isn't a terminal.
pub struct Progress {
    bar: Option<cliclack::ProgressBar>,
    total: u64,
    last_tenth: u64,
}

impl Progress {
    pub fn bytes(total: u64, message: impl Display) -> Self {
        if crate::decorated() {
            let bar = cliclack::progress_bar(total).with_download_template();
            bar.start(message);
            Self {
                bar: Some(bar),
                total,
                last_tenth: 0,
            }
        } else {
            eprintln!("{message} ({})", crate::bytes(total));
            Self {
                bar: None,
                total,
                last_tenth: 0,
            }
        }
    }

    pub fn set(&mut self, done: u64) {
        match &self.bar {
            Some(bar) => bar.set_position(done),
            None if self.total > 0 => {
                let tenth = done * 10 / self.total;
                if tenth > self.last_tenth {
                    self.last_tenth = tenth;
                    eprintln!("  {}%", tenth * 10);
                }
            }
            None => {}
        }
    }

    /// A side note while the bar runs (e.g. "resuming at byte N").
    pub fn note(&self, message: impl Display) {
        match &self.bar {
            Some(bar) => bar.set_message(message),
            None => eprintln!("  {message}"),
        }
    }

    pub fn done(self, message: impl Display) {
        match &self.bar {
            Some(bar) => bar.stop(message),
            None => eprintln!("{message}"),
        }
    }

    pub fn fail(self, message: impl Display) {
        match &self.bar {
            Some(bar) => bar.error(message),
            None => eprintln!("{message}"),
        }
    }
}

/// One line of a [`Watch`]: where an item is, and how far along.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchLine {
    pub status: String,
    pub tone: crate::Tone,
    /// 0-100, when the item reports it.
    pub percent: Option<u64>,
    pub detail: String,
    pub finished: bool,
}

/// Live lines for several long-running items at once (deployments being
/// installed), one progress bar each on a terminal; otherwise a plain line
/// whenever an item's line changes.
pub struct Watch {
    multi: Option<cliclack::MultiProgress>,
    bars: HashMap<String, cliclack::ProgressBar>,
    last: HashMap<String, WatchLine>,
}

impl Watch {
    pub fn new(title: impl Display) -> Self {
        let multi = crate::decorated().then(|| cliclack::multi_progress(title.to_string()));
        if multi.is_none() {
            eprintln!("{title}");
        }
        Self {
            multi,
            bars: HashMap::new(),
            last: HashMap::new(),
        }
    }

    pub fn update(&mut self, name: &str, line: WatchLine) {
        if self.last.get(name) == Some(&line) {
            return;
        }
        let message = format!(
            "{} {} {}",
            crate::accent(name),
            crate::toned(&line.status, line.tone),
            crate::dim(&line.detail)
        );
        match &self.multi {
            Some(multi) => {
                let bar = self.bars.entry(name.to_owned()).or_insert_with(|| {
                    let bar = multi.add(
                        cliclack::progress_bar(100)
                            .with_template("{msg} [{bar:20.cyan/blue}] {pos:>3}%"),
                    );
                    bar.start("");
                    bar
                });
                bar.set_position(line.percent.unwrap_or(0));
                if line.finished {
                    if line.tone == crate::Tone::Good {
                        bar.stop(message);
                    } else {
                        bar.error(message);
                    }
                } else {
                    bar.set_message(message);
                }
            }
            None => {
                let percent = line.percent.map(|p| format!(" {p}%")).unwrap_or_default();
                eprintln!("{name} {}{percent} {}", line.status, line.detail);
            }
        }
        self.last.insert(name.to_owned(), line);
    }

    pub fn finish(self) {
        if let Some(multi) = self.multi {
            multi.stop();
        }
    }
}
