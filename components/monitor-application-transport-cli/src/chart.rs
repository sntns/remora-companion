//! A device over time in a terminal: its metrics as braille line charts
//! (ratatui's, drawn into an off-screen buffer and printed -- the command
//! prints and returns, piped or not), its presence as a strip of blocks.

use std::time::SystemTime;

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Style},
    symbols::Marker,
    widgets::{Axis, Chart, Dataset, GraphType, Widget},
};
use remora_monitor::model::{Metric, Presence, Unit};
use remora_tui as tui;

use crate::service::span;

/// The color of a curve, and of what frames it: told apart when printing,
/// where they become the terminal's accent and dim styles.
const CURVE: Color = Color::Cyan;
const FRAME: Color = Color::DarkGray;

/// The upper ends a percentage's scale may take: the smallest that holds
/// the values, so that a device at rest is not a flat line at the bottom
/// of 0-100.
const PERCENT_SCALES: [f64; 5] = [5.0, 10.0, 25.0, 50.0, 100.0];

/// The scale a metric is drawn on: a percentage and a load from 0, up to
/// a round value above the largest; a temperature around what it was.
pub(crate) fn scale(unit: Unit, min: f64, max: f64) -> (f64, f64) {
    match unit {
        Unit::Percent => {
            let high = PERCENT_SCALES.iter().find(|high| max <= **high);
            (0.0, high.copied().unwrap_or(100.0))
        }
        Unit::Load => (0.0, max.ceil().max(1.0)),
        Unit::Bytes | Unit::BytesPerSecond => (0.0, round_up(max, 1024.0)),
        Unit::Milliseconds => (0.0, round_up(max, 1000.0)),
        Unit::Celsius | Unit::Dbm => {
            let low = (min / 10.0).floor() * 10.0;
            let high = (max / 10.0).ceil() * 10.0;
            (low, if high > low { high } else { low + 10.0 })
        }
    }
}

/// The first of 1, 2, 5, 10, 20, 50... -- times a power of `base`, the
/// step from a unit to the next -- that holds `max`.
fn round_up(max: f64, base: f64) -> f64 {
    if max <= 1.0 {
        return 1.0;
    }
    let mut unit = 1.0;
    while max > unit * base {
        unit *= base;
    }
    let steps = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0];
    let step = steps.iter().find(|step| max <= unit * **step);
    step.map_or(unit * base, |step| unit * step)
}

/// A number of bytes in the unit that suits it: `512 B`, `1.5 KiB`,
/// `20 MiB`.
pub(crate) fn bytes(bytes: f64) -> String {
    let mut value = bytes;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if value.abs() < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if unit == "B" || value.abs() >= 10.0 {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

/// The smallest and largest value; `(0, 0)` without any.
pub(crate) fn bounds(metric: &Metric) -> (f64, f64) {
    let values = metric.points.iter().map(|(_, value)| *value);
    let min = values.clone().reduce(f64::min).unwrap_or_default();
    let max = values.reduce(f64::max).unwrap_or_default();
    (min, max)
}

/// A metric's points as runs to join with a line, in seconds since
/// `from`: a run ends where the next point is later than one and a half
/// times the points' usual interval -- the device reported nothing
/// meanwhile, and a line across would say it did.
pub(crate) fn runs(metric: &Metric, from: SystemTime) -> Vec<Vec<(f64, f64)>> {
    let points: Vec<(f64, f64)> = metric
        .points
        .iter()
        .filter_map(|(at, value)| Some((at.duration_since(from).ok()?.as_secs_f64(), *value)))
        .collect();
    let mut intervals: Vec<f64> = points
        .windows(2)
        .map(|pair| pair[1].0 - pair[0].0)
        .collect();
    intervals.sort_by(f64::total_cmp);
    let usual = intervals.get(intervals.len() / 2).copied().unwrap_or(0.0);

    let mut runs: Vec<Vec<(f64, f64)>> = Vec::new();
    for point in points {
        match runs.last_mut() {
            Some(run)
                if run
                    .last()
                    .is_some_and(|last| point.0 - last.0 <= usual * 1.5) =>
            {
                run.push(point);
            }
            _ => runs.push(vec![point]),
        }
    }
    runs
}

/// Draws `metric` between `from` and `to` into a buffer `width` columns
/// by `height` rows: the curve, and the scale with its ends on the left.
/// [`chart`] draws the time axis under the plot.
pub(crate) fn draw(
    metric: &Metric,
    from: SystemTime,
    to: SystemTime,
    width: u16,
    height: u16,
) -> Buffer {
    let window = to.duration_since(from).unwrap_or_default().as_secs_f64();
    let runs = runs(metric, from);
    let chart = widget(metric, &runs, window, CURVE);
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    chart.render(area, &mut buffer);
    buffer
}

/// `metric`'s chart as a widget: its `runs` (see [`runs`]) as braille
/// lines in the `curve` color over a `window` of seconds, and the scale
/// with its ends on the left. No time axis: its labels would widen the
/// scale's margin to their own width; whoever places the chart draws it.
pub(crate) fn widget<'a>(
    metric: &Metric,
    runs: &'a [Vec<(f64, f64)>],
    window: f64,
    curve: Color,
) -> Chart<'a> {
    let (min, max) = bounds(metric);
    let (low, high) = scale(metric.unit, min, max);
    let datasets = runs
        .iter()
        .map(|run| {
            Dataset::default()
                .marker(Marker::Braille)
                // A lone point has no line to be on.
                .graph_type(if run.len() > 1 {
                    GraphType::Line
                } else {
                    GraphType::Scatter
                })
                .style(Style::new().fg(curve))
                .data(run)
        })
        .collect();
    let frame = Style::new().fg(FRAME);
    Chart::new(datasets)
        .x_axis(Axis::default().bounds([0.0, window.max(1.0)]).style(frame))
        .y_axis(
            Axis::default()
                .bounds([low, high])
                .labels([value(metric.unit, low), value(metric.unit, high)])
                .labels_alignment(Alignment::Right)
                .style(frame),
        )
}

/// A buffer's rows as lines of text for the terminal: the curve in the
/// accent color, the frame dim, trailing blanks dropped.
fn lines(buffer: &Buffer, plain: bool) -> Vec<String> {
    let area = buffer.area();
    (area.top()..area.bottom())
        .map(|y| {
            // Runs of cells of one color, styled as one.
            let mut line = String::new();
            let mut run = (Color::Reset, String::new());
            let flush = |(color, text): &mut (Color, String), line: &mut String| {
                if !text.is_empty() {
                    line.push_str(&match *color {
                        _ if plain => text.clone(),
                        CURVE => tui::accent(&*text),
                        FRAME => tui::dim(&*text),
                        _ => text.clone(),
                    });
                    text.clear();
                }
            };
            for x in area.left()..area.right() {
                let Some(cell) = buffer.cell((x, y)) else {
                    continue;
                };
                // Blanks carry no color of their own: they stay in the run.
                if cell.fg != run.0 && cell.symbol() != " " {
                    flush(&mut run, &mut line);
                    run.0 = cell.fg;
                }
                run.1.push_str(cell.symbol());
            }
            run.1.truncate(run.1.trim_end().len());
            flush(&mut run, &mut line);
            line
        })
        .collect()
}

/// A value with its unit: `12%`, `54 °C`, `0.42`.
pub(crate) fn value(unit: Unit, value: f64) -> String {
    match unit {
        Unit::Percent => format!("{value:.0}%"),
        Unit::Celsius => format!("{value:.0} °C"),
        Unit::Load => format!("{value:.2}"),
        Unit::Bytes => bytes(value),
        Unit::BytesPerSecond => format!("{}/s", bytes(value)),
        Unit::Dbm => format!("{value:.0} dBm"),
        Unit::Milliseconds => format!("{value:.0} ms"),
    }
}

/// `10-02 09:32` (local time): a chart's ends are days apart at most.
pub(crate) fn short_time(time: SystemTime) -> String {
    let full = tui::timestamp(time);
    match full.get(5..16) {
        Some(short) => format!("{short}{}", if full.ends_with('Z') { "Z" } else { "" }),
        None => full,
    }
}

/// Prints `metric`'s chart to stdout: its name and figures, then the
/// curve between its axes.
pub(crate) fn print(metric: &Metric, from: SystemTime, to: SystemTime, width: u16, height: u16) {
    let Some((_, last)) = metric.points.last() else {
        return;
    };
    let (min, max) = bounds(metric);
    let mean = metric.points.iter().map(|(_, v)| v).sum::<f64>() / metric.points.len() as f64;
    let unit = metric.unit;
    println!(
        "{}  {}",
        tui::accent(&metric.name),
        tui::dim(format!(
            "now {} · min {} · avg {} · max {}",
            value(unit, *last),
            value(unit, min),
            value(unit, mean),
            value(unit, max)
        ))
    );
    for line in chart(metric, from, to, width, height, false) {
        println!("{line}");
    }
}

/// `metric`'s chart as lines for the terminal, `width` columns wide: the
/// plot (`height` rows), then the time axis and its ends, under the plot
/// alone.
fn chart(
    metric: &Metric,
    from: SystemTime,
    to: SystemTime,
    width: u16,
    height: u16,
    plain: bool,
) -> Vec<String> {
    let buffer = draw(metric, from, to, width, height);
    // Where the scale's line is: left of it, the margin of its labels.
    let margin = (0..width)
        .find(|x| {
            buffer
                .cell((*x, 0))
                .is_some_and(|cell| cell.symbol() == "│")
        })
        .map_or(0, usize::from);
    let plot = usize::from(width).saturating_sub(margin + 1);
    let dim = |text: String| if plain { text } else { tui::dim(text) };
    let mut chart = lines(&buffer, plain);
    chart.push(dim(format!("{:margin$}└{}", "", "─".repeat(plot))));
    chart.push(dim(format!("{:margin$} {}", "", time_axis(from, to, plot))));
    chart
}

/// The labels of a time axis `width` columns wide: its ends, at its ends.
fn time_axis(from: SystemTime, to: SystemTime, width: usize) -> String {
    let (start, end) = (short_time(from), short_time(to));
    let gap = width.saturating_sub(start.chars().count() + end.chars().count());
    format!("{start}{}{end}", " ".repeat(gap))
}

/// How much of each of `slices` slices of time a device was there, from 0
/// (silent throughout) to 1 (there all along).
pub(crate) fn present(
    presence: &Presence,
    from: SystemTime,
    to: SystemTime,
    slices: usize,
) -> Vec<f64> {
    let span = to.duration_since(from).unwrap_or_default().as_secs_f64();
    let offset = |time: SystemTime| time.duration_since(from).unwrap_or_default().as_secs_f64();
    let slice = span / slices.max(1) as f64;
    (0..slices)
        .map(|index| {
            let (start, end) = (index as f64 * slice, (index + 1) as f64 * slice);
            let silent: f64 = presence
                .gaps
                .iter()
                .map(|(since, until)| {
                    (offset(*until).min(end) - offset(*since).max(start)).max(0.0)
                })
                .sum();
            if slice > 0.0 {
                (1.0 - silent / slice).clamp(0.0, 1.0)
            } else {
                1.0
            }
        })
        .collect()
}

/// A device's presence as one row, a block per slice of time: full when
/// it was there all along, lighter the more of the slice it was silent,
/// blank when silent throughout.
pub(crate) fn strip(
    presence: &Presence,
    from: SystemTime,
    to: SystemTime,
    slices: usize,
) -> String {
    present(presence, from, to, slices)
        .into_iter()
        .map(|present| match present {
            present if present >= 0.95 => '█',
            present if present >= 0.5 => '▓',
            present if present > 0.05 => '░',
            _ => ' ',
        })
        .collect()
}

/// Prints a device's presence to stdout: its figures, the strip, and the
/// time axis.
pub(crate) fn print_presence(presence: &Presence, from: SystemTime, to: SystemTime, width: usize) {
    let longest = presence
        .gaps
        .iter()
        .filter_map(|(since, until)| until.duration_since(*since).ok())
        .max();
    let silences = match (presence.gaps.len(), longest) {
        (1, Some(longest)) => format!("1 silence of {}", span(longest.as_secs())),
        (count, Some(longest)) => {
            format!("{count} silences, the longest {}", span(longest.as_secs()))
        }
        _ => "no silence".to_owned(),
    };
    println!(
        "{}  {}",
        tui::accent("presence"),
        tui::dim(format!(
            "{:.1}% · {} heartbeats, one every {} · {silences}",
            presence.availability_percent,
            presence.beats.len(),
            span(presence.interval.as_secs())
        ))
    );
    println!(
        "{}{}",
        tui::dim("│"),
        tui::accent(strip(presence, from, to, width))
    );
    println!(
        "{}",
        tui::dim(format!(
            "└{}\n{}",
            "─".repeat(width),
            time_axis(from, to, width + 1)
        ))
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn metric(unit: Unit, points: &[(u64, f64)]) -> Metric {
        Metric {
            name: "m".into(),
            unit,
            points: points.iter().map(|(t, v)| (at(*t), *v)).collect(),
        }
    }

    fn is_braille(character: char) -> bool {
        ('\u{2801}'..='\u{28ff}').contains(&character)
    }

    #[test]
    fn a_chart_has_its_curve_between_its_axes_and_their_labels() {
        let points: Vec<(u64, f64)> = (0..=20).map(|n| (n * 300, n as f64 * 5.0)).collect();
        let chart = chart(
            &metric(Unit::Percent, &points),
            at(0),
            at(6000),
            60,
            6,
            true,
        );
        for row in &chart {
            println!("{row}");
        }
        assert_eq!(chart.len(), 8);
        // The scale's ends, left of the plot, top and bottom.
        assert!(chart[0].starts_with("100%│"), "{chart:#?}");
        assert!(chart[5].starts_with("  0%│"), "{chart:#?}");
        // The time axis under the plot, and its ends at its ends.
        assert!(chart[6].starts_with("    └───"), "{chart:#?}");
        assert_eq!(chart[6].chars().count(), 60);
        let start = format!("     {} ", short_time(at(0)));
        assert!(chart[7].starts_with(&start), "{chart:#?}");
        let end = format!(" {}", short_time(at(6000)));
        assert!(chart[7].ends_with(&end), "{chart:#?}");
        assert_eq!(chart[7].chars().count(), 60);
        // The curve rises from the bottom left to the top right.
        let first = |row: &str| row.chars().position(is_braille);
        assert!(first(&chart[5]) < first(&chart[0]), "{chart:#?}");
        assert!(chart[..6].iter().all(|row| row.chars().any(is_braille)));
    }

    #[test]
    fn nothing_is_drawn_where_the_device_reported_nothing() {
        // Every 5 minutes, but for the middle half of the window.
        let points: Vec<(u64, f64)> = (0..=40)
            .filter(|n| !(10..30).contains(n))
            .map(|n| (n * 300, 50.0))
            .collect();
        let metric = metric(Unit::Percent, &points);
        let runs = runs(&metric, at(0));
        assert_eq!(
            runs.iter().map(Vec::len).collect::<Vec<_>>(),
            [10, 11],
            "{runs:?}"
        );
        let chart = lines(&draw(&metric, at(0), at(12_000), 64, 6), true);
        let row = chart
            .iter()
            .find(|row| row.chars().any(is_braille))
            .unwrap();
        let drawn: Vec<usize> = row
            .chars()
            .enumerate()
            .filter(|(_, c)| is_braille(*c))
            .map(|(index, _)| index)
            .collect();
        let widest_gap = drawn.windows(2).map(|pair| pair[1] - pair[0]).max();
        assert!(widest_gap > Some(20), "{row}");
    }

    #[test]
    fn a_lone_point_is_still_drawn() {
        let chart = lines(
            &draw(&metric(Unit::Load, &[(50, 0.5)]), at(0), at(100), 40, 6),
            true,
        );
        assert!(chart.iter().any(|row| row.chars().any(is_braille)));
    }

    #[test]
    fn a_presence_strip_fades_with_the_silence() {
        let presence = Presence {
            interval: Duration::from_secs(300),
            beats: vec![],
            // All of the second slice, most of the third, a little of the
            // fourth.
            gaps: vec![(at(100), at(270)), (at(390), at(400))],
            availability_percent: 0.0,
        };
        assert_eq!(strip(&presence, at(0), at(500), 5), "█ ░▓█");
    }

    #[test]
    fn a_scale_fits_its_unit_and_its_values() {
        assert_eq!(scale(Unit::Percent, 0.0, 1.2), (0.0, 5.0));
        assert_eq!(scale(Unit::Percent, 9.0, 24.0), (0.0, 25.0));
        assert_eq!(scale(Unit::Percent, 40.0, 60.0), (0.0, 100.0));
        assert_eq!(scale(Unit::Percent, 0.0, 100.0), (0.0, 100.0));
        assert_eq!(scale(Unit::Load, 0.2, 0.4), (0.0, 1.0));
        assert_eq!(scale(Unit::Load, 0.2, 3.2), (0.0, 4.0));
        assert_eq!(scale(Unit::Celsius, 41.0, 58.5), (40.0, 60.0));
        assert_eq!(scale(Unit::Celsius, 50.0, 50.0), (50.0, 60.0));
        assert_eq!(scale(Unit::Dbm, -71.0, -58.0), (-80.0, -50.0));
        assert_eq!(scale(Unit::Milliseconds, 31.0, 311.0), (0.0, 500.0));
        assert_eq!(scale(Unit::Milliseconds, 0.0, 0.0), (0.0, 1.0));
        assert_eq!(
            scale(Unit::BytesPerSecond, 0.0, 3000.0),
            (0.0, 5.0 * 1024.0)
        );
        assert_eq!(
            scale(Unit::Bytes, 0.0, 120e6),
            (0.0, 200.0 * 1024.0 * 1024.0)
        );
    }

    #[test]
    fn values_carry_their_unit() {
        assert_eq!(value(Unit::Percent, 12.4), "12%");
        assert_eq!(value(Unit::Celsius, 53.6), "54 °C");
        assert_eq!(value(Unit::Load, 0.416), "0.42");
        assert_eq!(value(Unit::Bytes, 512.0), "512 B");
        assert_eq!(value(Unit::Bytes, 20_684_800.0), "20 MiB");
        assert_eq!(value(Unit::BytesPerSecond, 1536.0), "1.5 KiB/s");
        assert_eq!(value(Unit::Dbm, -58.0), "-58 dBm");
        assert_eq!(value(Unit::Milliseconds, 52.4), "52 ms");
        assert_eq!(
            short_time(at(1_790_000_000)),
            tui::timestamp(at(1_790_000_000))[5..16]
        );
    }
}
