use comfy_table::{presets, Attribute, Cell, Color, ContentArrangement};

/// A borderless, docker-style table for stdout: bold dim headers, one row
/// per item, columns sized to the terminal.
pub struct Table(comfy_table::Table);

impl Table {
    pub fn new<I, S>(headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut table = comfy_table::Table::new();
        table
            .load_preset(presets::NOTHING)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(headers.into_iter().map(|header| {
                Cell::new(header.into().to_uppercase())
                    .add_attribute(Attribute::Bold)
                    .fg(Color::DarkGrey)
            }));
        // No left padding, three spaces between columns, like `docker ps`.
        for column in table.column_iter_mut() {
            column.set_padding((0, 3));
        }
        Self(table)
    }

    /// A row; a `highlighted` row (e.g. the current context) is drawn in
    /// the accent color.
    pub fn row<I, S>(&mut self, cells: I, highlighted: bool) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.0.add_row(cells.into_iter().map(|cell| {
            let cell = Cell::new(cell.into());
            if highlighted {
                cell.fg(Color::Cyan).add_attribute(Attribute::Bold)
            } else {
                cell
            }
        }));
        self
    }

    /// A row whose cells each carry their own tone (`None` is plain).
    pub fn row_toned<I, S>(&mut self, cells: I) -> &mut Self
    where
        I: IntoIterator<Item = (S, Option<crate::Tone>)>,
        S: Into<String>,
    {
        self.0.add_row(cells.into_iter().map(|(cell, tone)| {
            let cell = Cell::new(cell.into());
            match tone {
                None => cell,
                Some(crate::Tone::Good) => cell.fg(Color::Green).add_attribute(Attribute::Bold),
                Some(crate::Tone::Bad) => cell.fg(Color::Red).add_attribute(Attribute::Bold),
                Some(crate::Tone::Active) => cell.fg(Color::Cyan),
                Some(crate::Tone::Idle) => cell.fg(Color::DarkGrey),
                Some(crate::Tone::Warn) => cell.fg(Color::Yellow),
            }
        }));
        self
    }

    /// Prints the table to stdout. Colors are dropped when stdout is not a
    /// terminal, so piping it through `grep`/`awk` sees plain text.
    pub fn print(&mut self) {
        let stdout = console::Term::stdout();
        if !stdout.is_term() {
            self.0.force_no_tty();
        }
        // Wrapping to a width nobody reported (a pty sized 0) would stack
        // every cell one letter per line; better one long line.
        if stdout
            .size_checked()
            .is_none_or(|(_, columns)| columns < 40)
        {
            self.0.set_content_arrangement(ContentArrangement::Disabled);
        }
        // comfy-table pads every line to its column width; trailing spaces
        // only get in the way of anything reading the output.
        for line in self.0.to_string().lines() {
            println!("{}", line.trim_end());
        }
    }
}
