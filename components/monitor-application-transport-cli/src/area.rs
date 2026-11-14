//! What `watch` paints with: colors as the terminal can show them,
//! gradients, and a metric as a filled braille graph shaded from its foot
//! to its top.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

pub(crate) type Rgb = (u8, u8, u8);

/// How colors reach the terminal.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Theme {
    /// 24-bit colors; otherwise the nearest of the 256.
    pub truecolor: bool,
}

impl Theme {
    /// As the terminal says of itself (`COLORTERM`).
    pub(crate) fn detect() -> Self {
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        Self {
            truecolor: colorterm.contains("truecolor") || colorterm.contains("24bit"),
        }
    }

    pub(crate) fn color(self, (red, green, blue): Rgb) -> Color {
        if self.truecolor {
            return Color::Rgb(red, green, blue);
        }
        // The 6x6x6 cube of the 256 colors.
        let level = |channel: u8| (u16::from(channel) * 5 + 127) / 255;
        let index = 16 + 36 * level(red) + 6 * level(green) + level(blue);
        Color::Indexed(u8::try_from(index).unwrap_or(u8::MAX))
    }
}

/// Styles in the terminal's colors.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Paint(pub Theme);

impl Paint {
    pub(crate) fn fg(self, color: Rgb) -> Style {
        Style::new().fg(self.0.color(color))
    }

    pub(crate) fn bold(self, color: Rgb) -> Style {
        self.fg(color).add_modifier(Modifier::BOLD)
    }

    pub(crate) fn dim(self) -> Style {
        self.fg(DIM)
    }
}

pub(crate) const TITLE: Rgb = (0xee, 0xee, 0xee);
pub(crate) const TEXT: Rgb = (0xc8, 0xc8, 0xd0);
pub(crate) const DIM: Rgb = (0x80, 0x80, 0x8c);
/// The part of a meter its value does not reach; what is behind the
/// document picked.
pub(crate) const OFF: Rgb = (0x40, 0x40, 0x4a);
pub(crate) const ACCENT: Rgb = (0x6c, 0xc4, 0xe8);
pub(crate) const GOOD: Rgb = (0x77, 0xca, 0x9b);
pub(crate) const WARN: Rgb = (0xcb, 0xc0, 0x6c);
pub(crate) const BAD: Rgb = (0xdc, 0x4c, 0x4c);
/// From fine to alarming: a meter along its length, a value by how high
/// it is.
pub(crate) const THRESHOLD: Gradient = Gradient(&[GOOD, WARN, BAD]);

/// Colors to go through, evenly spread from 0 to 1.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Gradient(pub &'static [Rgb]);

impl Gradient {
    /// The color at `position`, from 0 (the first) to 1 (the last).
    pub(crate) fn at(self, position: f64) -> Rgb {
        let Some(last) = self.0.len().checked_sub(1) else {
            return (0, 0, 0);
        };
        let scaled = position.clamp(0.0, 1.0) * last as f64;
        let index = (scaled.floor() as usize).min(last);
        let (from, to) = (self.0[index], self.0[(index + 1).min(last)]);
        let part = scaled - index as f64;
        let mix =
            |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * part).round() as u8;
        (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
    }
}

/// The value to draw in each of `count` columns across `window` seconds:
/// the largest of the points falling in a column, a line between two
/// columns of a same run, nothing between runs (see [`crate::chart::runs`]).
pub(crate) fn columns(runs: &[Vec<(f64, f64)>], window: f64, count: usize) -> Vec<Option<f64>> {
    let mut columns = vec![None; count];
    let Some(last) = count.checked_sub(1) else {
        return columns;
    };
    let column = |seconds: f64| ((seconds / window.max(1.0) * count as f64) as usize).min(last);
    for run in runs {
        let mut previous: Option<(usize, f64)> = None;
        for (seconds, value) in run {
            let index = column(*seconds);
            let value = columns[index].map_or(*value, |known: f64| known.max(*value));
            columns[index] = Some(value);
            if let Some((before, from)) = previous.filter(|(before, _)| *before + 1 < index) {
                let steps = (index - before) as f64;
                for (between, column) in columns.iter_mut().enumerate().take(index).skip(before + 1)
                {
                    let part = (between - before) as f64 / steps;
                    *column = Some(from + (value - from) * part);
                }
            }
            previous = Some((index, value));
        }
    }
    columns
}

/// A metric as a filled graph: under its curve, braille dots (two columns
/// and four levels a cell), each row of cells a step further along the
/// gradient.
pub(crate) struct Area<'a> {
    pub runs: &'a [Vec<(f64, f64)>],
    /// Seconds across.
    pub window: f64,
    /// The values at the foot and at the top.
    pub low: f64,
    pub high: f64,
    pub gradient: Gradient,
    pub theme: Theme,
}

/// The bit of a braille cell's dot, by column (left, right) and level
/// (from the foot).
const DOTS: [[u32; 4]; 2] = [[0x40, 0x04, 0x02, 0x01], [0x80, 0x20, 0x10, 0x08]];

impl Widget for Area<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let (width, height) = (usize::from(area.width), usize::from(area.height));
        if width == 0 || height == 0 {
            return;
        }
        let levels = height * 4;
        let range = (self.high - self.low).max(f64::EPSILON);
        // How many dots high each column is: one at least, the foot of the
        // graph where the device reported.
        let heights: Vec<usize> = columns(self.runs, self.window, width * 2)
            .into_iter()
            .map(|value| {
                value.map_or(0, |value| {
                    let part = ((value - self.low) / range).clamp(0.0, 1.0);
                    ((part * levels as f64).round() as usize).max(1)
                })
            })
            .collect();
        for row in 0..height {
            let color = self
                .theme
                .color(self.gradient.at((row + 1) as f64 / height as f64));
            for cell in 0..width {
                let mut dots = 0;
                for (side, bits) in DOTS.iter().enumerate() {
                    let filled = heights[cell * 2 + side].saturating_sub(row * 4);
                    for bit in bits.iter().take(filled) {
                        dots |= bit;
                    }
                }
                if dots == 0 {
                    continue;
                }
                let (x, y) = (
                    area.x + u16::try_from(cell).unwrap_or(u16::MAX),
                    area.bottom() - 1 - u16::try_from(row).unwrap_or(u16::MAX),
                );
                if let (Some(symbol), Some(target)) =
                    (char::from_u32(0x2800 + dots), buffer.cell_mut((x, y)))
                {
                    target.set_char(symbol).set_fg(color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: Theme = Theme { truecolor: true };
    const GREY: Gradient = Gradient(&[(0, 0, 0), (100, 100, 100), (200, 200, 200)]);

    fn rows(area: Area, width: u16, height: u16) -> (Vec<String>, Buffer) {
        let rect = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(rect);
        area.render(rect, &mut buffer);
        let rows = (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell((x, y)).map(|cell| cell.symbol().to_owned()))
                    .collect()
            })
            .collect();
        (rows, buffer)
    }

    #[test]
    fn a_gradient_goes_through_its_colors() {
        assert_eq!(GREY.at(0.0), (0, 0, 0));
        assert_eq!(GREY.at(0.25), (50, 50, 50));
        assert_eq!(GREY.at(0.5), (100, 100, 100));
        assert_eq!(GREY.at(1.0), (200, 200, 200));
        assert_eq!(GREY.at(7.0), (200, 200, 200));
    }

    #[test]
    fn colors_fall_back_to_the_256() {
        assert_eq!(THEME.color((255, 0, 0)), Color::Rgb(255, 0, 0));
        let indexed = Theme { truecolor: false };
        assert_eq!(indexed.color((255, 0, 0)), Color::Indexed(196));
        assert_eq!(indexed.color((255, 255, 255)), Color::Indexed(231));
    }

    #[test]
    fn columns_join_a_run_and_leave_a_silence_blank() {
        // Two runs, eight columns of 10 s: points at columns 0 and 3, then 6.
        let runs = vec![vec![(0.0, 10.0), (35.0, 40.0)], vec![(65.0, 5.0)]];
        assert_eq!(
            columns(&runs, 80.0, 8),
            [
                Some(10.0),
                Some(20.0),
                Some(30.0),
                Some(40.0),
                None,
                None,
                Some(5.0),
                None
            ]
        );
        // Several points in a column: the largest.
        let runs = vec![vec![(0.0, 1.0), (1.0, 9.0), (2.0, 3.0)]];
        assert_eq!(columns(&runs, 80.0, 8)[0], Some(9.0));
    }

    #[test]
    fn an_area_is_filled_under_its_curve_and_shaded_by_row() {
        // From nothing to everything across four cells, two rows high.
        let runs = vec![vec![(0.0, 0.0), (79.0, 100.0)]];
        let (rows, buffer) = rows(
            Area {
                runs: &runs,
                window: 80.0,
                low: 0.0,
                high: 100.0,
                gradient: GREY,
                theme: THEME,
            },
            4,
            2,
        );
        // The top row only where the curve is in the upper half; the foot
        // everywhere, full under the upper half.
        assert!(rows[0].starts_with("  "), "{rows:?}");
        assert!(rows[0].ends_with('⣿') || rows[0].ends_with('⣾'), "{rows:?}");
        assert!(rows[1].ends_with("⣿⣿"), "{rows:?}");
        assert!(!rows[1].contains(' '), "{rows:?}");
        let color = |x, y| buffer.cell((x, y)).map(|cell| cell.fg);
        assert_eq!(color(3, 1), Some(Color::Rgb(100, 100, 100)));
        assert_eq!(color(3, 0), Some(Color::Rgb(200, 200, 200)));
    }
}
