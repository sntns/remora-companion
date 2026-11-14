//! `rmra monitor watch`: a device full screen and in color -- its presence,
//! what it last reported, and one of its documents in detail with its
//! numbers over time -- read again as it reports.

use std::{
    future::Future,
    io::IsTerminal,
    pin::Pin,
    time::{Duration, SystemTime},
};

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use error_stack::{Report, ResultExt};
use futures_util::StreamExt;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph, Row, Table},
    Frame,
};
use remora_context::model::ContextOverride;
use remora_monitor::{
    application::{Error as Monitor, MonitorService},
    model::{
        is_silent, metrics, presence, system_metrics, Document, Metric, Presence, Unit, ACCESS,
        HEARTBEAT, NETWORK, PROCESSES, SYSTEM,
    },
};
use remora_tui as tui;

use crate::{
    area::{
        Area, Gradient, Paint, Rgb, Theme, ACCENT, BAD, GOOD, OFF, TEXT, THRESHOLD, TITLE, WARN,
    },
    chart, detail,
    error::{Error, Result},
    service::{history, span, summary, Target},
};

/// How often the screen is drawn again without news: ages go by.
const CLOCK: Duration = Duration::from_secs(1);
/// The narrowest a terminal may be for boxes side by side.
const WIDE: u16 = 110;
/// How wide the documents' box is, beside the one picked: names and ages.
const LIST_WIDTH: u16 = 28;
/// The narrowest a graph's box may be for its figures in its frame.
const FIGURES_WIDTH: u16 = 60;
/// The rows the graphs keep when what is over them would take them all.
const GRAPHS_HEIGHT: u16 = 8;

/// The windows a key picks, and how the footer names them.
const WINDOWS: [(char, &str, u64); 5] = [
    ('1', "1h", 3600),
    ('6', "6h", 6 * 3600),
    ('d', "24h", 86_400),
    ('w', "7d", 7 * 86_400),
    ('m', "30d", 30 * 86_400),
];

/// The frames of the boxes.
const DEVICE_BOX: Rgb = (0x55, 0x6d, 0x59);
const DOCUMENTS_BOX: Rgb = (0x5c, 0x58, 0x8d);
const DETAIL_BOX: Rgb = (0x6c, 0x6c, 0x4b);

/// A metric's own hue, by what it measures (`rx wlan0`: `rx`), the same
/// from a read to the next: its graph goes from the darkest at its foot
/// to the lightest at its top.
fn hue(name: &str) -> Gradient {
    match name.split(' ').next().unwrap_or(name) {
        "cpu" | "rx" => Gradient(&[(0x1f, 0x6f, 0x5c), (0x3f, 0xc4, 0x9a), (0x9d, 0xf5, 0xc8)]),
        "load" | "tx" => Gradient(&[(0x5a, 0x3a, 0x9a), (0x9a, 0x6c, 0xe0), (0xd8, 0xb4, 0xff)]),
        "memory" | "rtt" | "channel" => {
            Gradient(&[(0x24, 0x55, 0xa8), (0x4a, 0x90, 0xe2), (0x9c, 0xd0, 0xff)])
        }
        "temperature" => Gradient(&[(0x9a, 0x30, 0x30), (0xe0, 0x60, 0x48), (0xff, 0xb0, 0x80)]),
        _ => Gradient(&[(0x80, 0x5c, 0x14), (0xd0, 0x9a, 0x2c), (0xf8, 0xdc, 0x7c)]),
    }
}

/// Whether the documents named `name` have numbers over time to graph.
fn has_metrics(name: &str) -> bool {
    [SYSTEM, NETWORK, ACCESS, PROCESSES].contains(&name)
}

/// A device as last read from the platform.
pub(crate) struct Snapshot {
    pub latest: Vec<Document>,
    pub presence: Option<Presence>,
    /// The document whose history was read, and its numbers over time.
    pub document: Option<String>,
    pub metrics: Vec<Metric>,
    /// The window it covers; `to` is when it was read.
    pub from: SystemTime,
    pub to: SystemTime,
}

/// What the screen shows.
pub(crate) struct State {
    pub device: String,
    pub since: Duration,
    /// The document shown in detail; the `system` one, or the first,
    /// until one is picked.
    pub picked: Option<String>,
    pub snapshot: Option<Snapshot>,
    /// Why the last read failed, until one succeeds.
    pub error: Option<String>,
    pub reading: bool,
    /// How colors reach the terminal.
    pub theme: Theme,
}

impl State {
    /// The document to show in detail, of those last reported.
    fn shown(&self) -> Option<&Document> {
        let latest = &self.snapshot.as_ref()?.latest;
        let named = |name: &str| latest.iter().find(|document| document.name == name);
        self.picked
            .as_deref()
            .and_then(named)
            .or_else(|| named(SYSTEM))
            .or_else(|| latest.first())
    }

    /// Picks the document `step` rows from the one shown, around the ends.
    fn pick(&mut self, step: isize) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let count = snapshot.latest.len();
        let Some(shown) = self.shown() else {
            return;
        };
        let index = snapshot
            .latest
            .iter()
            .position(|document| document.name == shown.name)
            .unwrap_or(0);
        let next = (index as isize + step).rem_euclid(count as isize) as usize;
        self.picked = snapshot
            .latest
            .get(next)
            .map(|document| document.name.clone());
    }

    /// Whether the history read is not the one of the document shown,
    /// which has numbers over time.
    fn lacks_history(&self) -> bool {
        self.shown().is_some_and(|shown| {
            has_metrics(&shown.name)
                && self
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.document.as_deref())
                    != Some(shown.name.as_str())
        })
    }
}

type Read = std::result::Result<Snapshot, Report<Monitor>>;

/// Reads the device's latest documents, its heartbeats over `since`, and
/// the history of the document `picked` (the `system` one by default) when
/// it has numbers over time.
async fn read(
    service: &MonitorService,
    over: Option<&ContextOverride>,
    target: &Target,
    since: Duration,
    picked: Option<String>,
) -> Read {
    let to = SystemTime::now();
    let from = to.checked_sub(since).unwrap_or(SystemTime::UNIX_EPOCH);
    let latest = service.latest(over, &target.bucket, &target.device).await?;
    let heartbeats = history(service, over, target, HEARTBEAT, from).await?;
    let reported = |name: &str| latest.iter().any(|document| document.name == name);
    let document = picked
        .filter(|picked| reported(picked))
        .or_else(|| reported(SYSTEM).then(|| SYSTEM.to_owned()));
    let metrics = match document.as_deref().filter(|name| has_metrics(name)) {
        Some(name) => metrics(name, &history(service, over, target, name, from).await?),
        None => Vec::new(),
    };
    Ok(Snapshot {
        latest,
        presence: presence(&heartbeats, from, to),
        document,
        metrics,
        from,
        to,
    })
}

/// What a key asks for.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    Refresh,
    Window(Duration),
    /// The document so many rows down the list.
    Pick(isize),
}

fn action(key: KeyEvent) -> Option<Action> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => Some(Action::Pick(1)),
        KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => Some(Action::Pick(-1)),
        KeyCode::Char(pressed) => WINDOWS
            .iter()
            .find(|(key, _, _)| *key == pressed)
            .map(|(_, _, seconds)| Action::Window(Duration::from_secs(*seconds))),
        _ => None,
    }
}

/// Puts the terminal back as it was, however the screen is left.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

pub(crate) async fn run(
    service: &MonitorService,
    over: Option<&ContextOverride>,
    target: &Target,
    since: Duration,
    every: Duration,
    document: Option<String>,
) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(Report::new(Error::NotATerminal));
    }
    let mut terminal = ratatui::try_init().change_context(Error::Terminal)?;
    let _restore = Restore;
    let mut events = EventStream::new();
    let mut clock = tokio::time::interval(CLOCK);

    let mut state = State {
        device: target.device.clone(),
        since,
        picked: document,
        snapshot: None,
        error: None,
        reading: true,
        theme: Theme::detect(),
    };
    // The read under way, if any: the screen stays live meanwhile.
    let mut reading: Option<Pin<Box<dyn Future<Output = Read> + '_>>> = Some(Box::pin(read(
        service,
        over,
        target,
        state.since,
        state.picked.clone(),
    )));
    let mut next = tokio::time::Instant::now() + every;

    loop {
        terminal
            .draw(|frame| draw(frame, &state, SystemTime::now()))
            .change_context(Error::Terminal)?;
        let again = tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => match action(key) {
                    Some(Action::Quit) => break,
                    Some(Action::Refresh) => true,
                    Some(Action::Window(since)) => {
                        state.since = since;
                        true
                    }
                    Some(Action::Pick(step)) => {
                        state.pick(step);
                        // Its history is to read, when it has one to graph.
                        state.lacks_history()
                    }
                    None => false,
                },
                // A resize, the mouse...: drawn again is all it takes.
                Some(Ok(_)) => false,
                Some(Err(error)) => {
                    return Err(Report::new(error).change_context(Error::Terminal))
                }
                None => break,
            },
            read = async {
                match reading.as_mut() {
                    Some(reading) => reading.await,
                    None => std::future::pending().await,
                }
            } => {
                reading = None;
                state.reading = false;
                next = tokio::time::Instant::now() + every;
                match read {
                    Ok(snapshot) => {
                        state.snapshot = Some(snapshot);
                        state.error = None;
                    }
                    Err(report) => state.error = Some(tui::one_line(&report, false)),
                }
                false
            }
            () = tokio::time::sleep_until(next), if reading.is_none() => true,
            _ = clock.tick() => false,
        };
        if again {
            state.reading = true;
            let picked = state
                .shown()
                .map(|shown| shown.name.clone())
                .or(state.picked.clone());
            reading = Some(Box::pin(read(service, over, target, state.since, picked)));
        }
    }
    Ok(())
}

/// Tags set in a box's frame, a stretch of frame between them:
/// `┤ cpu ├─┤ 12% ├`.
fn tags(paint: Paint, frame: Rgb, tags: Vec<Vec<Span<'static>>>) -> Line<'static> {
    let style = paint.fg(frame);
    let mut line = Vec::new();
    for (index, tag) in tags.into_iter().enumerate() {
        if index > 0 {
            line.push(Span::styled("─", style));
        }
        line.push(Span::styled("┤ ", style));
        line.extend(tag);
        line.push(Span::styled(" ├", style));
    }
    Line::from(line)
}

/// A box with round corners, what it holds named in its frame.
fn boxed(paint: Paint, frame: Rgb, named: Vec<Vec<Span<'static>>>) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(paint.fg(frame))
        .title_top(tags(paint, frame, named))
}

/// The whole screen: the device and its presence; what it last reported,
/// beside the document picked in detail; a graph per number that
/// document has over time; and the keys.
pub(crate) fn draw(frame: &mut Frame, state: &State, now: SystemTime) {
    let paint = Paint(state.theme);
    let area = frame.area();
    let [device, body, footer] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_device(frame, device, state, now, paint);
    frame.render_widget(keys(state, paint), footer);
    let (Some(snapshot), Some(shown)) = (&state.snapshot, state.shown()) else {
        return;
    };

    let rows = |count: usize, around: u16| u16::try_from(count).unwrap_or(u16::MAX) + around;
    let now_metrics = system_metrics(std::slice::from_ref(shown));
    let lines = if shown.name == SYSTEM {
        Vec::new()
    } else {
        detail::lines(shown, paint)
    };
    let list = rows(snapshot.latest.len(), 3);
    let detail = if shown.name == SYSTEM {
        rows(now_metrics.len(), 3)
    } else {
        rows(lines.len(), 2)
    };
    let graphs = has_metrics(&shown.name);
    let beside = area.width >= WIDE;
    // Without graphs, the document picked has the rest of the screen.
    let upper = if beside {
        list.max(detail)
    } else {
        list + detail
    };
    let upper = if graphs {
        Constraint::Length(upper.min(body.height.saturating_sub(GRAPHS_HEIGHT)))
    } else {
        Constraint::Fill(1)
    };
    let [upper, lower] = Layout::vertical([upper, Constraint::Fill(u16::from(graphs))]).areas(body);
    let [list, detail] = if beside {
        Layout::horizontal([Constraint::Length(LIST_WIDTH), Constraint::Fill(1)]).areas(upper)
    } else {
        Layout::vertical([Constraint::Length(list), Constraint::Fill(1)]).areas(upper)
    };

    draw_documents(frame, list, snapshot, shown, now, paint, !beside);
    let block = boxed(
        paint,
        DETAIL_BOX,
        vec![
            vec![Span::styled(shown.name.clone(), paint.bold(TITLE))],
            vec![Span::styled(tui::timestamp(shown.captured_at), paint.dim())],
        ],
    );
    let inner = block.inner(detail);
    frame.render_widget(block, detail);
    if shown.name == SYSTEM {
        draw_system(frame, inner, shown, &now_metrics, snapshot, paint);
    } else {
        frame.render_widget(Paragraph::new(lines), inner);
    }
    if graphs {
        // The history read is another document's until the next read.
        if state.lacks_history() {
            let wait = Line::from(Span::styled(" reading its history…", paint.dim()));
            frame.render_widget(wait, lower);
        } else {
            draw_graphs(frame, lower, snapshot, &shown.name, paint);
        }
    }
}

/// The keys, the window shown standing out.
fn keys(state: &State, paint: Paint) -> Line<'static> {
    let key = |key: &str| Span::styled(key.to_owned(), paint.bold(ACCENT));
    let mut line = vec![
        Span::raw(" "),
        key("q"),
        Span::styled(" quit  ", paint.dim()),
        key("r"),
        Span::styled(" read again  ", paint.dim()),
        key("↑↓"),
        Span::styled(" document  ", paint.dim()),
        Span::styled("│ ", paint.fg(OFF)),
        Span::styled("window  ", paint.dim()),
    ];
    for (window, name, seconds) in WINDOWS {
        line.push(key(&window.to_string()));
        line.push(Span::styled(
            format!(" {name}  "),
            if state.since.as_secs() == seconds {
                paint.bold(TITLE)
            } else {
                paint.dim()
            },
        ));
    }
    Line::from(line)
}

/// How good an availability is.
fn availability_color(percent: f64) -> Rgb {
    match percent {
        percent if percent >= 99.0 => GOOD,
        percent if percent >= 95.0 => WARN,
        _ => BAD,
    }
}

/// The presence strip, a block per slice of time: the more of it the
/// device was there, the taller and the greener.
fn presence_strip(
    presence: &Presence,
    from: SystemTime,
    to: SystemTime,
    width: usize,
    paint: Paint,
) -> Line<'static> {
    const BLOCKS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    Line::from(
        chart::present(presence, from, to, width)
            .into_iter()
            .map(|present| {
                let block = BLOCKS[((present * 7.0).round() as usize).min(7)];
                Span::styled(block, paint.fg(THRESHOLD.at(1.0 - present)))
            })
            .collect::<Vec<_>>(),
    )
}

/// The device's box: its name, the window and how fresh what is shown is
/// in its frame; its presence inside.
fn draw_device(frame: &mut Frame, area: Rect, state: &State, now: SystemTime, paint: Paint) {
    let mut fresh = vec![vec![Span::styled(
        format!("last {}", span(state.since.as_secs())),
        paint.fg(TEXT),
    )]];
    match (&state.error, &state.snapshot) {
        (Some(error), _) => fresh.push(vec![Span::styled(error.clone(), paint.bold(BAD))]),
        (None, Some(snapshot)) => {
            let age = now.duration_since(snapshot.to).unwrap_or_default();
            fresh.push(vec![Span::styled(
                format!("read {} ago", span(age.as_secs())),
                paint.dim(),
            )]);
        }
        (None, None) => {}
    }
    if state.reading {
        fresh.push(vec![Span::styled("reading…", paint.fg(WARN))]);
    }
    let block = boxed(
        paint,
        DEVICE_BOX,
        vec![
            vec![Span::styled(state.device.clone(), paint.bold(ACCENT))],
            vec![Span::styled("presence", paint.bold(TITLE))],
        ],
    )
    .title_top(tags(paint, DEVICE_BOX, fresh).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(snapshot) = &state.snapshot else {
        return;
    };
    let Some(presence) = &snapshot.presence else {
        let none = Line::from(Span::styled("no heartbeat in this window", paint.dim()));
        frame.render_widget(Paragraph::new(none), inner);
        return;
    };
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
    let figures = Line::from(vec![
        Span::styled(
            format!("{:.1}%", presence.availability_percent),
            paint.bold(availability_color(presence.availability_percent)),
        ),
        Span::styled(
            format!(
                " · {} heartbeats, one every {} · {silences}",
                presence.beats.len(),
                span(presence.interval.as_secs())
            ),
            paint.dim(),
        ),
    ]);
    let strip = presence_strip(
        presence,
        snapshot.from,
        snapshot.to,
        usize::from(inner.width),
        paint,
    );
    frame.render_widget(Paragraph::new(vec![figures, strip]), inner);
}

/// How alarming a value is, from 0 to 1; a load, a size, a delay say
/// nothing of it by themselves.
fn alarm(unit: Unit, value: f64) -> Option<f64> {
    match unit {
        Unit::Percent => Some(value / 100.0),
        Unit::Celsius => Some((value - 40.0) / 50.0),
        Unit::Dbm => Some((-55.0 - value) / 35.0),
        Unit::Load | Unit::Bytes | Unit::BytesPerSecond | Unit::Milliseconds => None,
    }
}

/// The color of a metric's value: by how alarming it is, its own hue
/// otherwise.
fn value_color(metric: &Metric, value: f64) -> Rgb {
    alarm(metric.unit, value).map_or_else(|| hue(&metric.name).at(1.0), |alarm| THRESHOLD.at(alarm))
}

/// A meter `width` cells long, lit up to `part` (0 to 1) of its length:
/// green at its start, red at its end.
fn meter(part: f64, width: u16, paint: Paint) -> Line<'static> {
    // A cell at least for a value above nothing.
    let lit = (part.clamp(0.0, 1.0) * f64::from(width)).round() as u16;
    let lit = if part > 0.0 { lit.max(1) } else { lit };
    Line::from(
        (0..width)
            .map(|cell| {
                let along = f64::from(cell) / f64::from(width.saturating_sub(1).max(1));
                Span::styled(
                    "■",
                    paint.fg(if cell < lit { THRESHOLD.at(along) } else { OFF }),
                )
            })
            .collect::<Vec<_>>(),
    )
}

/// The `system` document: what runs and since when, then its metrics
/// (`now`, from that document alone), a line each -- a meter for what has
/// a full scale, the window in small for a load.
fn draw_system(
    frame: &mut Frame,
    area: Rect,
    document: &Document,
    now: &[Metric],
    snapshot: &Snapshot,
    paint: Paint,
) {
    let [runs, meters] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let (body, boot) = (&document.body, &document.body["boot"]);
    let mut running: Vec<String> = ["version", "slot"]
        .iter()
        .filter_map(|field| boot[field].as_str().map(str::to_owned))
        .collect();
    if let Some(seconds) = body["uptime-seconds"].as_u64() {
        running.push(format!("up {}", span(seconds)));
    }
    if let Some(count) = body["cpu"]["count"].as_u64() {
        running.push(format!("{count} processors"));
    }
    if let Some(action) = boot["reboot"]["action"].as_str() {
        running.push(format!("last boot: {action}"));
    }
    frame.render_widget(
        Line::from(Span::styled(running.join(" · "), paint.dim())),
        runs,
    );

    let names = now
        .iter()
        .map(|metric| metric.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(usize::from(meters.width / 2));
    let window = snapshot
        .to
        .duration_since(snapshot.from)
        .unwrap_or_default()
        .as_secs_f64();
    for (metric, row) in now.iter().zip(meters.rows()) {
        let Some((_, last)) = metric.points.last() else {
            continue;
        };
        let [name, _, bar, figure] = Layout::horizontal([
            Constraint::Length(u16::try_from(names).unwrap_or(u16::MAX)),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(7),
        ])
        .areas(row);
        let gradient = hue(&metric.name);
        frame.render_widget(
            Span::styled(metric.name.clone(), paint.bold(gradient.at(1.0))),
            name,
        );
        // The same metric over the window, when it is the one read.
        let over_time = snapshot
            .metrics
            .iter()
            .find(|read| read.name == metric.name && snapshot.document.as_deref() == Some(SYSTEM));
        match (metric.unit, over_time) {
            (Unit::Percent | Unit::Celsius, _) => {
                frame.render_widget(meter(*last / 100.0, bar.width, paint), bar);
            }
            (_, Some(over_time)) => {
                let (min, max) = chart::bounds(over_time);
                let (low, high) = chart::scale(over_time.unit, min, max);
                frame.render_widget(
                    Area {
                        runs: &chart::runs(over_time, snapshot.from),
                        window,
                        low,
                        high,
                        gradient,
                        theme: paint.0,
                    },
                    bar,
                );
            }
            _ => {}
        }
        frame.render_widget(
            Line::from(Span::styled(
                chart::value(metric.unit, *last),
                paint.bold(value_color(metric, *last)),
            ))
            .right_aligned(),
            figure,
        );
    }
}

/// How fresh a document is: the heartbeat by its own interval, any other
/// by the hour and the day.
fn age_color(document: &Document, now: SystemTime) -> Rgb {
    if document.name == HEARTBEAT {
        return if is_silent(document, now) { BAD } else { GOOD };
    }
    match now
        .duration_since(document.captured_at)
        .unwrap_or_default()
        .as_secs()
    {
        0..=3599 => GOOD,
        3600..=86_399 => WARN,
        _ => BAD,
    }
}

/// `text` in `width` columns at most, cut short with an ellipsis.
fn fit(text: String, width: usize) -> String {
    if text.chars().count() <= width {
        return text;
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

const NAME_WIDTH: u16 = 10;
const AGE_WIDTH: u16 = 12;

/// What the device last reported, a document a row: its name, its age,
/// its summary where there is room (`summaries`); the one `shown` in
/// detail standing out.
fn draw_documents(
    frame: &mut Frame,
    area: Rect,
    snapshot: &Snapshot,
    shown: &Document,
    now: SystemTime,
    paint: Paint,
    summaries: bool,
) {
    // What the frame, the other columns and the gaps leave to the summary.
    let room = usize::from(area.width.saturating_sub(NAME_WIDTH + AGE_WIDTH + 6));
    let rows = snapshot.latest.iter().map(|document| {
        let age = now.duration_since(document.captured_at).unwrap_or_default();
        let picked = document.name == shown.name;
        let row = Row::new([
            Span::styled(
                document.name.clone(),
                paint.bold(if picked { ACCENT } else { TITLE }),
            ),
            Span::styled(
                format!("{} ago", span(age.as_secs())),
                paint.fg(age_color(document, now)),
            ),
            Span::styled(
                if summaries {
                    fit(summary(document, now), room)
                } else {
                    String::new()
                },
                paint.fg(TEXT),
            ),
        ]);
        if picked {
            row.style(Style::new().bg(paint.0.color(OFF)))
        } else {
            row
        }
    });
    let header = if summaries {
        ["DOCUMENT", "REPORTED", "SUMMARY"]
    } else {
        ["DOCUMENT", "REPORTED", ""]
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(NAME_WIDTH),
            Constraint::Length(AGE_WIDTH),
            Constraint::Fill(1),
        ],
    )
    .header(Row::new(header).style(paint.dim()))
    .column_spacing(2)
    .block(boxed(
        paint,
        DOCUMENTS_BOX,
        vec![vec![Span::styled("last reported", paint.bold(TITLE))]],
    ));
    frame.render_widget(table, area);
}

/// The graphs, side by side on a wide terminal: each in its box, its name
/// and last value in the frame, with its figures and the window's ends.
fn draw_graphs(frame: &mut Frame, area: Rect, snapshot: &Snapshot, document: &str, paint: Paint) {
    let metrics = &snapshot.metrics;
    if metrics.is_empty() {
        let none = Line::from(Span::styled(
            format!(" nothing to graph of {document} in this window"),
            paint.dim(),
        ));
        frame.render_widget(Paragraph::new(none), area);
        return;
    }
    let columns = if area.width >= WIDE && metrics.len() > 1 {
        2
    } else {
        1
    };
    let window = snapshot
        .to
        .duration_since(snapshot.from)
        .unwrap_or_default()
        .as_secs_f64();
    let rows =
        Layout::vertical(vec![Constraint::Fill(1); metrics.len().div_ceil(columns)]).split(area);
    for (index, metric) in metrics.iter().enumerate() {
        let Some(row) = rows.get(index / columns) else {
            continue;
        };
        let cells = Layout::horizontal(vec![Constraint::Fill(1); columns]).split(*row);
        if let Some(cell) = cells.get(index % columns) {
            draw_graph(frame, *cell, metric, snapshot, window, paint);
        }
    }
}

fn draw_graph(
    frame: &mut Frame,
    area: Rect,
    metric: &Metric,
    snapshot: &Snapshot,
    window: f64,
    paint: Paint,
) {
    let gradient = hue(&metric.name);
    let frame_color = gradient.at(0.0);
    let unit = metric.unit;
    let (min, max) = chart::bounds(metric);
    let last = metric.points.last().map_or(0.0, |(_, value)| *value);
    let mean =
        metric.points.iter().map(|(_, v)| v).sum::<f64>() / metric.points.len().max(1) as f64;
    let end =
        |time: SystemTime| Span::styled(format!(" {} ", chart::short_time(time)), paint.dim());
    let mut block = boxed(
        paint,
        frame_color,
        vec![
            vec![Span::styled(
                metric.name.clone(),
                paint.bold(gradient.at(1.0)),
            )],
            vec![Span::styled(
                chart::value(unit, last),
                paint.bold(value_color(metric, last)),
            )],
        ],
    )
    .title_bottom(Line::from(end(snapshot.from)).left_aligned())
    .title_bottom(Line::from(end(snapshot.to)).right_aligned());
    if area.width >= FIGURES_WIDTH {
        block = block.title_top(
            Line::from(Span::styled(
                format!(
                    " min {} · avg {} · max {} ",
                    chart::value(unit, min),
                    chart::value(unit, mean),
                    chart::value(unit, max)
                ),
                paint.dim(),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    // The scale's ends, left of the graph: the top one, and the foot one.
    let (low, high) = chart::scale(unit, min, max);
    let (top, foot) = (chart::value(unit, high), chart::value(unit, low));
    let margin = top.chars().count().max(foot.chars().count());
    let [labels, _, plot] = Layout::horizontal([
        Constraint::Length(u16::try_from(margin).unwrap_or(u16::MAX)),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);
    let label = |text: String| Line::from(Span::styled(text, paint.dim())).right_aligned();
    frame.render_widget(
        label(foot),
        Rect {
            y: labels.bottom() - 1,
            height: 1,
            ..labels
        },
    );
    frame.render_widget(
        label(top),
        Rect {
            height: 1,
            ..labels
        },
    );
    frame.render_widget(
        Area {
            runs: &chart::runs(metric, snapshot.from),
            window,
            low,
            high,
            gradient,
            theme: paint.0,
        },
        plot,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, style::Color, Terminal};
    use serde_json::json;

    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn document(name: &str, captured: u64, body: serde_json::Value) -> Document {
        Document {
            name: name.into(),
            schema: format!("remora-monitoring-{name}-v1"),
            captured_at: at(captured),
            boot_id: None,
            seq: None,
            body,
        }
    }

    fn system(captured: u64, cpu: f64) -> Document {
        document(
            SYSTEM,
            captured,
            json!({
                "uptime-seconds": 7200,
                "boot": {"version": "release-1.2.1", "slot": "slot-A"},
                "cpu": {"count": 4, "usage-percent": cpu, "load": [0.5, 0.4, 0.3]},
                "memory": {"total-bytes": 1000, "available-bytes": 600},
            }),
        )
    }

    /// A day of a device: present but for two hours, its `system`
    /// documents read.
    fn snapshot() -> Snapshot {
        let beats: Vec<Document> = (0..=288u64)
            .filter(|n| !(100..124).contains(n))
            .map(|n| document(HEARTBEAT, n * 300, json!({"interval": 300})))
            .collect();
        let history: Vec<Document> = (0..=288u64)
            .map(|n| system(n * 300, if n == 288 { 38.0 } else { (n % 50) as f64 }))
            .collect();
        Snapshot {
            latest: vec![
                document(
                    "health",
                    86_000,
                    json!({"state": "healthy", "checks": [{"name": "heartbeat", "value": true}]}),
                ),
                document(HEARTBEAT, 86_400, json!({"interval": 300})),
                document(
                    NETWORK,
                    86_100,
                    json!({
                        "internet": {
                            "connection": "Home", "gateway": "192.0.2.1", "interface": "wlan0",
                        },
                        "interfaces": [{
                            "name": "wlan0", "operstate": "up", "addresses": [], "counters": {},
                        }],
                    }),
                ),
                document("ota", 1000, json!({"phase": "idle"})),
                system(86_400, 38.0),
            ],
            presence: presence(&beats, at(0), at(86_400)),
            document: Some(SYSTEM.into()),
            metrics: metrics(SYSTEM, &history),
            from: at(0),
            to: at(86_400),
        }
    }

    fn state(snapshot: Option<Snapshot>) -> State {
        State {
            device: "DEV1".into(),
            since: Duration::from_secs(86_400),
            picked: None,
            snapshot,
            error: None,
            reading: false,
            theme: Theme { truecolor: true },
        }
    }

    /// The screen as text, and the terminal it was drawn on.
    fn screen(state: &State, width: u16, height: u16) -> (Vec<String>, Terminal<TestBackend>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, state, at(86_430)))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines = (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell((x, y)).map(|cell| cell.symbol().to_owned()))
                    .collect::<String>()
            })
            .collect();
        (lines, terminal)
    }

    /// The colors of the cells showing `symbol`, in the order of the screen.
    fn colors_of(terminal: &Terminal<TestBackend>, symbol: &str) -> Vec<Color> {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|cell| cell.symbol() == symbol)
            .map(|cell| cell.fg)
            .collect()
    }

    fn rgb((red, green, blue): Rgb) -> Color {
        Color::Rgb(red, green, blue)
    }

    #[test]
    fn the_screen_shows_the_device_in_color() {
        let (lines, terminal) = screen(&state(Some(snapshot())), 120, 40);
        let text = lines.join("\n");
        // The device's box: its name, the window and the read's age in its
        // frame.
        assert!(lines[0].starts_with("╭┤ DEV1 ├─┤ presence ├──"), "{text}");
        assert!(
            lines[0].ends_with("─┤ last 1d ├─┤ read 30s ago ├╮"),
            "{text}"
        );
        // Its presence: figures, and a strip tall and green where there,
        // low and red where silent.
        assert!(
            text.contains("91.7% · 265 heartbeats, one every 5m · 1 silence of 2h"),
            "{text}"
        );
        assert_eq!(colors_of(&terminal, "█").first(), Some(&rgb(GOOD)));
        assert_eq!(colors_of(&terminal, "▁").first(), Some(&rgb(BAD)));
        // Its documents with their age, beside the `system` one in detail:
        // what runs, and a meter per metric lit as far as its value.
        let list = lines
            .iter()
            .position(|line| line.starts_with("╭┤ last reported ├"))
            .expect(&text);
        assert!(
            lines[list].contains("╮╭┤ system ├─┤ 1970-01-02 00:00:00Z ├"),
            "{text}"
        );
        assert!(
            lines[list + 1].contains("││release-1.2.1 · slot-A · up 2h · 4 processors"),
            "{text}"
        );
        assert!(lines[list + 2].starts_with("│health      7m ago"), "{text}");
        assert!(lines[list + 2].contains("││cpu    ■■■"), "{text}");
        assert!(lines[list + 2].ends_with("■    38%│"), "{text}");
        assert!(
            lines[list + 3].starts_with("│heartbeat   30s ago"),
            "{text}"
        );
        assert!(
            lines[list + 5].starts_with("│ota         23h 43m ago"),
            "{text}"
        );
        assert!(lines[list + 4].contains("││memory ■■■"), "{text}");
        let meters = colors_of(&terminal, "■");
        assert_eq!(meters.first(), Some(&rgb(GOOD)));
        assert!(meters.contains(&rgb(OFF)), "{meters:?}");
        // Its graphs side by side: name, last value and figures in the
        // frame, the window's ends under, the curve filled and shaded.
        let titles = lines
            .iter()
            .find(|line| line.starts_with("╭┤ cpu ├─┤ 38% ├"))
            .expect(&text);
        assert!(titles.contains("╮╭┤ load ├─┤ 0.50 ├"), "{text}");
        assert!(titles.contains(" min 0% · avg 24% · max 49% ╮"), "{text}");
        assert!(text.contains("╭┤ memory ├─┤ 40% ├"), "{text}");
        assert!(lines[38].starts_with("╰ 01-01 00:00Z ─"), "{text}");
        assert!(lines[38].contains("─ 01-02 00:00Z ╯"), "{text}");
        let mut shades = colors_of(&terminal, "⣿");
        shades.dedup();
        assert!(shades.len() > 5, "{shades:?}");
        // The keys, the window shown standing out.
        assert!(
            lines[39]
                .starts_with(" q quit  r read again  ↑↓ document  │ window  1 1h  6 6h  d 24h"),
            "{text}"
        );
    }

    #[test]
    fn a_document_picked_is_shown_in_detail() {
        let mut state = state(Some(snapshot()));
        // From `system`, the last: down goes around to the first.
        state.pick(1);
        assert_eq!(state.picked.as_deref(), Some("health"));
        state.pick(2);
        assert_eq!(state.picked.as_deref(), Some(NETWORK));
        let (lines, _) = screen(&state, 120, 40);
        let text = lines.join("\n");
        assert!(
            text.contains("╭┤ network ├─┤ 1970-01-01 23:55:00Z ├"),
            "{text}"
        );
        assert!(
            text.contains("││internet  Home via 192.0.2.1 on wlan0"),
            "{text}"
        );
        assert!(text.contains("││wlan0      up "), "{text}");
        // Its history is not the one read: no other document's graphs.
        assert!(state.lacks_history());
        assert!(text.contains(" reading its history…"), "{text}");
        assert!(!text.contains("┤ cpu ├"), "{text}");

        // A document without numbers over time has the rest of the screen.
        state.pick(-2);
        assert!(!state.lacks_history());
        let (lines, _) = screen(&state, 120, 40);
        let text = lines.join("\n");
        assert!(text.contains("╭┤ health ├"), "{text}");
        assert!(
            text.contains("││checks  name=heartbeat, value=true"),
            "{text}"
        );
        assert!(text.contains("││state   healthy"), "{text}");
        assert!(!text.contains("reading its history"), "{text}");
        assert!(lines[38].starts_with("╰─"), "{text}");

        // One the device no longer reports: back to `system`.
        state.picked = Some("gone".into());
        assert_eq!(state.shown().map(|shown| shown.name.as_str()), Some(SYSTEM));
    }

    #[test]
    fn a_narrow_terminal_stacks_its_boxes() {
        let (lines, _) = screen(&state(Some(snapshot())), 80, 50);
        let text = lines.join("\n");
        let at = |start: &str| lines.iter().position(|line| line.starts_with(start));
        let (documents, detail) = (at("╭┤ last reported ├"), at("╭┤ system ├"));
        let (cpu, memory) = (at("╭┤ cpu ├"), at("╭┤ memory ├"));
        assert!(documents.is_some() && documents < detail, "{text}");
        assert!(detail < cpu && cpu < memory, "{text}");
        // The documents have their summary there.
        assert!(
            text.contains("health      7m ago        state=healthy, checks=1"),
            "{text}"
        );
        // A summary too long for its column says it was cut short.
        assert_eq!(fit("state=established".into(), 12), "state=estab…");
        assert_eq!(fit("state=ok".into(), 12), "state=ok");
    }

    #[test]
    fn a_failed_read_says_so_over_what_was_last_read() {
        let mut state = state(Some(snapshot()));
        state.error = Some("failed to reach the gateway".into());
        state.reading = true;
        let (lines, terminal) = screen(&state, 120, 40);
        assert!(
            lines[0].ends_with("┤ last 1d ├─┤ failed to reach the gateway ├─┤ reading… ├╮"),
            "{}",
            lines[0]
        );
        assert!(lines.join("\n").contains("91.7%"));
        assert_eq!(colors_of(&terminal, "f").first(), Some(&rgb(BAD)));

        // Nothing read yet: the device's box and the keys alone.
        let (lines, _) = screen(
            &super::State {
                snapshot: None,
                ..state
            },
            120,
            40,
        );
        assert!(lines[0].starts_with("╭┤ DEV1 ├"));
        assert!(lines[4].trim().is_empty());
        assert!(lines[39].starts_with(" q quit"));
    }

    #[test]
    fn keys_quit_read_again_pick_a_window_and_a_document() {
        let press = |code| action(KeyEvent::new(code, KeyModifiers::NONE));
        assert_eq!(press(KeyCode::Char('q')), Some(Action::Quit));
        assert_eq!(press(KeyCode::Esc), Some(Action::Quit));
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        assert_eq!(press(KeyCode::Char('r')), Some(Action::Refresh));
        assert_eq!(
            press(KeyCode::Char('w')),
            Some(Action::Window(Duration::from_secs(7 * 86_400)))
        );
        assert_eq!(press(KeyCode::Down), Some(Action::Pick(1)));
        assert_eq!(press(KeyCode::Tab), Some(Action::Pick(1)));
        assert_eq!(press(KeyCode::Up), Some(Action::Pick(-1)));
        assert_eq!(press(KeyCode::Char('x')), None);
    }
}
