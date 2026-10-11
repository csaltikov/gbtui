//! Drawing. Everything reads from `App`; the only state written back is the
//! track width, so zoom limits follow the terminal size.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row as TableRow, Table, Wrap};

use super::app::{App, Mode, SortKey, Track};
use super::gc::GcCounts;
use super::tracks::{density, location_string, spark, tick_spacing};
use crate::model::{Feature, FeatureKind, Strand, Topology};
use crate::thousands;

/// Columns left of the tracks for the strand labels.
const GUTTER: u16 = 3;
/// Lanes drawn per strand; extra lanes are folded into the last one.
const MAX_LANES: usize = 4;
/// Smallest window the GC skew is computed over; the skew of a few bases
/// is noise.
const MIN_SKEW_WINDOW: usize = 1000;
/// Skew that fills a bar when the view's own range is smaller, so flat
/// regions don't get blown up into full-height noise.
const MIN_SKEW_SCALE: f64 = 0.05;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.set_cols(area.width.saturating_sub(GUTTER) as usize);

    let forward_rows = app.layout().forward_lanes.clamp(1, MAX_LANES) as u16;
    let reverse_rows = app.layout().reverse_lanes.clamp(1, MAX_LANES) as u16;
    let gc_rows = u16::from(app.settings.gc_skew && app.layout().gc.is_some());

    let [
        header,
        overview,
        ruler,
        forward,
        sequence,
        reverse,
        gc_skew,
        details,
        status,
    ] = Split::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(forward_rows),
        Constraint::Length(2),
        Constraint::Length(reverse_rows),
        Constraint::Length(gc_rows),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(frame, app, header);
    draw_overview(frame, app, overview);
    draw_ruler(frame, app, ruler);
    draw_track(frame, app, forward, Track::Forward, "+");
    draw_sequence(frame, app, sequence);
    draw_track(frame, app, reverse, Track::Reverse, "-");
    draw_gc_skew(frame, app, gc_skew);
    if app.list_open {
        // With room to spare, keep a few lines of details under the list.
        if details.height >= 16 {
            let [list, below] =
                Split::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .areas(details);
            draw_feature_list(frame, app, list);
            draw_details(frame, app, below);
        } else {
            draw_feature_list(frame, app, details);
        }
    } else {
        draw_details(frame, app, details);
    }
    draw_status(frame, app, status);

    if app.mode == Mode::Help {
        draw_help(frame, area);
    }
}

/// One row of styled cells, merged into spans when drawn.
struct Row {
    cells: Vec<(char, Style)>,
}

impl Row {
    fn new(width: usize) -> Self {
        Row {
            cells: vec![(' ', Style::default()); width],
        }
    }

    fn put(&mut self, col: usize, ch: char, style: Style) {
        if let Some(cell) = self.cells.get_mut(col) {
            *cell = (ch, style);
        }
    }

    fn text(&mut self, col: usize, text: &str, style: Style) {
        for (i, ch) in text.chars().enumerate() {
            self.put(col + i, ch, style);
        }
    }

    fn into_line(self, prefix: &str) -> Line<'static> {
        let mut spans = vec![Span::styled(
            format!("{prefix:<width$}", width = GUTTER as usize),
            Style::default().dim(),
        )];
        let mut text = String::new();
        let mut style = None;
        for (ch, s) in self.cells {
            if style.is_some_and(|prev| prev != s) {
                spans.push(Span::styled(std::mem::take(&mut text), style.unwrap()));
            }
            style = Some(s);
            text.push(ch);
        }
        if let Some(s) = style {
            spans.push(Span::styled(text, s));
        }
        Line::from(spans)
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let record = app.record();
    let mut left = format!(" {}  {} bp", record.name, thousands(record.len()));
    if record.topology == Topology::Circular {
        left.push_str("  circular");
    }
    if app.records.len() > 1 {
        left.push_str(&format!(
            "  [record {}/{}]",
            app.current + 1,
            app.records.len()
        ));
    }

    let window = app.window();
    let bpc = app.bp_per_col();
    let scale = if bpc >= 10.0 {
        format!("{} bp/col", thousands(bpc.round() as usize))
    } else {
        format!("{bpc:.2} bp/col")
    };
    let right = format!(
        "{}..{}  ({} bp, {scale}) ",
        thousands(window.start + 1),
        thousands(window.end),
        thousands(window.len())
    );

    let style = Style::default().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(left).style(style.bold()), area);
    frame.render_widget(
        Paragraph::new(right)
            .style(style)
            .alignment(Alignment::Right),
        area,
    );
}

fn draw_overview(frame: &mut Frame, app: &App, area: Rect) {
    let cols = app.cols;
    let record = app.record();
    let len = record.len().max(1);
    let layout = app.layout();

    let counts = density(
        layout
            .shown
            .iter()
            .map(|&i| record.features[i].location.start()),
        len,
        cols,
    );
    let max = counts.iter().copied().max().unwrap_or(0);

    let col_of = |pos: usize| (pos as u128 * cols as u128 / len as u128) as usize;
    let window = app.window();
    let (view_start, view_end) = (
        col_of(window.start),
        col_of(window.end).max(col_of(window.start) + 1),
    );

    let mut row = Row::new(cols);
    for (col, &count) in counts.iter().enumerate() {
        let in_view = (view_start..view_end).contains(&col);
        let style = if in_view {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        row.put(col, spark(count, max), style);
    }
    frame.render_widget(Paragraph::new(row.into_line("")), area);
}

fn draw_ruler(frame: &mut Frame, app: &App, area: Rect) {
    let cols = app.cols;
    let bpc = app.bp_per_col();
    let window = app.window();
    let spacing = tick_spacing(bpc, 12);

    let mut labels = Row::new(cols);
    let mut ticks = Row::new(cols);
    for col in 0..cols {
        ticks.put(col, '─', Style::default().dim());
    }

    // Ticks at 1-based multiples of `spacing`.
    let mut pos = (window.start + 1).div_ceil(spacing) * spacing;
    let mut next_free = 0;
    while pos <= window.end {
        let col = ((pos - 1 - window.start) as f64 / bpc) as usize;
        ticks.put(col, '┴', Style::default());
        let label = thousands(pos);
        if col >= next_free && col + label.len() <= cols {
            labels.text(col, &label, Style::default());
            next_free = col + label.len() + 1;
        }
        pos += spacing;
    }

    frame.render_widget(
        Paragraph::new(vec![labels.into_line(""), ticks.into_line("")]),
        area,
    );
}

fn feature_color(kind: &FeatureKind) -> Color {
    match kind {
        FeatureKind::Cds => Color::Cyan,
        FeatureKind::Trna => Color::Magenta,
        FeatureKind::Rrna => Color::Yellow,
        FeatureKind::Mrna => Color::Blue,
        FeatureKind::Gene => Color::Green,
        FeatureKind::Other(key) if key.ends_with("RNA") => Color::Blue,
        _ => Color::Gray,
    }
}

fn draw_track(frame: &mut Frame, app: &App, area: Rect, track: Track, label: &str) {
    let cols = app.cols;
    let rows = area.height as usize;
    if rows == 0 {
        return;
    }
    let record = app.record();
    let layout = app.layout();
    let window = app.window();
    let bpc = app.bp_per_col();

    let col_start = |pos: usize| ((pos as f64 - window.start as f64) / bpc).floor();
    let col_end = |pos: usize| ((pos as f64 - window.start as f64) / bpc).ceil();
    let clip = |c: f64| c.clamp(0.0, cols as f64) as usize;

    let mut lanes: Vec<Row> = (0..rows).map(|_| Row::new(cols)).collect();
    let visible: Vec<usize> = layout
        .in_window(record, window.clone())
        .filter(|&k| layout.placement[k].0 == track)
        .collect();
    let style_of = |k: usize, feature: &Feature| {
        let mut style = Style::default().fg(feature_color(&feature.kind));
        if app.is_match(k) {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if app.selected == Some(k) {
            style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        style
    };

    // Two passes, so a neighbour's bar never covers an arrow or label.
    for &k in &visible {
        let row = &mut lanes[layout.placement[k].1.min(rows - 1)];
        let feature = &record.features[layout.shown[k]];
        let style = style_of(k, feature);

        // Segments low to high, with thin connectors between them.
        let mut segments = feature.location.segments.clone();
        segments.sort_by_key(|s| s.start);
        let mut prev_end: Option<usize> = None;
        for s in &segments {
            let (a, b) = (
                col_start(s.start),
                col_end(s.end).max(col_start(s.start) + 1.0),
            );
            if let Some(p) = prev_end {
                for col in p..clip(a) {
                    row.put(col, '─', style);
                }
            }
            for col in clip(a)..clip(b) {
                row.put(col, '━', style);
            }
            prev_end = Some(clip(b));
        }
    }

    for &k in &visible {
        let row = &mut lanes[layout.placement[k].1.min(rows - 1)];
        let feature = &record.features[layout.shown[k]];
        let style = style_of(k, feature);

        let (first, last) = (
            col_start(feature.location.start()),
            col_end(feature.location.end()).max(col_start(feature.location.start()) + 1.0),
        );
        let (vis_a, vis_b) = (clip(first), clip(last));
        let width = vis_b.saturating_sub(vis_a);

        // Direction arrow at the 3' end, if that end is on screen and the
        // feature is wide enough for the arrow not to be clutter.
        let extent = feature.location.end() - feature.location.start();
        if extent as f64 / bpc >= 3.0 {
            match feature.location.strand {
                Strand::Forward if last <= cols as f64 => row.put(vis_b - 1, '▶', style),
                Strand::Reverse if first >= 0.0 => row.put(vis_a, '◀', style),
                _ => {}
            }
        }

        // Label inside the bar: the whole label or nothing, then as much of
        // the product as fits. Keep two cells free at each end.
        let room = width.saturating_sub(4);
        if let Some(label) = feature.label().filter(|l| l.chars().count() <= room) {
            let mut text = label.to_string();
            let product = feature.qualifier("product").filter(|&p| p != label);
            let left = room - label.chars().count();
            if let Some(product) = product.filter(|_| left >= 6) {
                text.push(' ');
                text.push_str(&truncate(product, left - 1));
            }
            let text = format!(" {text} ");
            let offset = (width - text.chars().count()) / 2;
            row.text(vis_a + offset, &text, style);
        }
    }

    let lines: Vec<Line> = lanes
        .into_iter()
        .enumerate()
        .map(|(i, row)| row.into_line(if i == 0 { label } else { "" }))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Cut `text` to at most `max` characters, ending in `…` if shortened.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

fn base_color(base: u8) -> Color {
    match base.to_ascii_uppercase() {
        b'A' => Color::Green,
        b'C' => Color::Blue,
        b'G' => Color::Yellow,
        b'T' | b'U' => Color::Red,
        _ => Color::DarkGray,
    }
}

fn complement(base: u8) -> u8 {
    match base.to_ascii_uppercase() {
        b'A' => b'T',
        b'T' | b'U' => b'A',
        b'C' => b'G',
        b'G' => b'C',
        other => other,
    }
}

fn draw_sequence(frame: &mut Frame, app: &App, area: Rect) {
    let record = app.record();
    let window = app.window();

    if record.sequence.is_empty() {
        return;
    }
    if window.len() > app.cols {
        let hint = format!(
            "{:>w$}sequence shows at 1 bp/col (press + to zoom in)",
            "",
            w = GUTTER as usize
        );
        frame.render_widget(
            Paragraph::new(vec![Line::from(""), Line::from(hint).dim()]),
            area,
        );
        return;
    }

    let bases = &record.sequence[window.clone()];
    let mut top = Row::new(app.cols);
    let mut bottom = Row::new(app.cols);
    for (col, &b) in bases.iter().enumerate() {
        top.put(
            col,
            b.to_ascii_uppercase() as char,
            Style::default().fg(base_color(b)),
        );
        let c = complement(b);
        bottom.put(col, c as char, Style::default().fg(base_color(c)).dim());
    }
    frame.render_widget(
        Paragraph::new(vec![top.into_line("5'"), bottom.into_line("3'")]),
        area,
    );
}

/// GC skew per column: bar height is the size of the skew, colour its sign
/// (green for G > C, magenta for C > G).
fn draw_gc_skew(frame: &mut Frame, app: &App, area: Rect) {
    let Some(gc) = &app.layout().gc else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let seq = &app.record().sequence;
    let window = app.window();
    let bpc = app.bp_per_col();
    let half = (bpc.max(MIN_SKEW_WINDOW as f64) / 2.0) as usize;

    let values: Vec<Option<f64>> = (0..app.cols)
        .map(|col| {
            let center = window.start + ((col as f64 + 0.5) * bpc) as usize;
            gc.skew(seq, center.saturating_sub(half)..center + half)
        })
        .collect();
    let scale = values
        .iter()
        .flatten()
        .fold(MIN_SKEW_SCALE, |m, v| m.max(v.abs()));

    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let mut row = Row::new(app.cols);
    for (col, value) in values.iter().enumerate() {
        let Some(v) = *value else { continue };
        let level = ((v.abs() / scale) * BARS.len() as f64).ceil() as usize;
        let color = if v >= 0.0 {
            Color::Green
        } else {
            Color::Magenta
        };
        row.put(
            col,
            BARS[level.clamp(1, BARS.len()) - 1],
            Style::default().fg(color),
        );
    }
    frame.render_widget(Paragraph::new(row.into_line("GC")), area);
}

fn draw_details(frame: &mut Frame, app: &App, area: Rect) {
    let record = app.record();
    let (title, lines) = match app.selected_feature() {
        Some(feature) => feature_details(feature),
        None => {
            let mut lines = Vec::new();
            if let Some(definition) = &record.definition {
                lines.push(Line::from(definition.clone()));
            }
            if let Some(organism) = &record.organism {
                lines.push(Line::from(organism.lineage.join(" > ")).dim());
            }
            lines.push(Line::from(format!(
                "{} features shown",
                thousands(app.layout().shown.len())
            )));
            if let Some(gc) = &app.layout().gc {
                lines.push(Line::from(gc_summary(gc, record.topology)));
            }
            lines.push(Line::from(""));
            lines.push(
                Line::from(
                    "Tab: select next feature   f: feature list   /: search   g: go to   \
                     Backspace: back   c: GC skew on/off   ?: help",
                )
                .dim(),
            );
            (format!(" {} ", record.name), lines)
        }
    };

    let block = Block::default().borders(Borders::TOP).title(title.bold());
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        area,
    );
}

/// Where the cumulative GC skew bottoms out and peaks. On a circular
/// bacterial chromosome these mark the likely origin and terminus.
fn gc_summary(gc: &GcCounts, topology: Topology) -> String {
    let (Some(min), Some(max)) = (gc.cumulative_min, gc.cumulative_max) else {
        return "GC skew: no G or C in this sequence".to_string();
    };
    let (min, max) = (thousands(min + 1), thousands(max + 1));
    match topology {
        Topology::Circular => format!(
            "Cumulative GC skew: minimum at {min} (likely origin), maximum at {max} (likely terminus)"
        ),
        Topology::Linear => {
            format!("Cumulative GC skew: minimum at {min}, maximum at {max}")
        }
    }
}

fn feature_details(feature: &Feature) -> (String, Vec<Line<'static>>) {
    let loc = &feature.location;
    let length: usize = loc.segments.iter().map(|s| s.len()).sum();
    let strand = match loc.strand {
        Strand::Forward => "+",
        Strand::Reverse => "-",
        Strand::Unknown => "?",
    };

    let mut lines = vec![Line::from(vec![
        Span::styled(
            feature.kind.as_key().to_string(),
            Style::default().fg(feature_color(&feature.kind)).bold(),
        ),
        Span::raw(format!(
            "  {}  {} bp  strand {strand}",
            location_string(loc),
            thousands(length)
        )),
    ])];

    // Group repeated keys (db_xref) onto one line, keeping first-seen order.
    let mut keys: Vec<&str> = Vec::new();
    for (key, _) in &feature.qualifiers {
        if !keys.contains(&key.as_str()) {
            keys.push(key);
        }
    }
    let width = keys.iter().map(|k| k.len()).max().unwrap_or(0);
    for key in keys {
        let values: Vec<&str> = feature
            .qualifiers
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_deref().unwrap_or(""))
            .collect();
        lines.push(Line::from(vec![
            Span::styled(format!("{key:<width$}  "), Style::default().dim()),
            Span::raw(values.join(", ")),
        ]));
    }

    let title = format!(" {} ", feature.label().unwrap_or(feature.kind.as_key()));
    (title, lines)
}

fn draw_feature_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.list_rows();
    let cursor = app.list_cursor(&rows);
    // Border line + header row.
    app.list_page = area.height.saturating_sub(2) as usize;

    let record = app.record();
    let shown = &app.layout().shown;
    let features: Vec<&Feature> = rows.iter().map(|&k| &record.features[shown[k]]).collect();

    let arrow = if app.sort_reverse { "▼" } else { "▲" };
    let title = match app.search_query() {
        Some(query) => format!(
            " {} matches for \"{query}\" · sorted by {} {arrow} ",
            thousands(rows.len()),
            app.sort.name()
        ),
        None => format!(
            " Features ({}) · sorted by {} {arrow} ",
            thousands(rows.len()),
            app.sort.name()
        ),
    };

    let heading = |name: &str, key: Option<SortKey>| {
        if key == Some(app.sort) {
            format!("{name} {arrow}")
        } else {
            name.to_string()
        }
    };
    // Numbers are right-aligned so their digits line up.
    let right = |text: String| Cell::from(Line::from(text).alignment(Alignment::Right));
    let header = TableRow::new([
        right(heading("start", Some(SortKey::Position))),
        right(heading("end", None)),
        right(heading("length", Some(SortKey::Length))),
        Cell::from("±"),
        Cell::from(heading("type", Some(SortKey::Type))),
        Cell::from(heading("name", Some(SortKey::Name))),
        Cell::from(heading("product", Some(SortKey::Product))),
    ])
    .style(Style::default().bold().dim());

    let table_rows = features.iter().map(|f| {
        let loc = &f.location;
        let strand = match loc.strand {
            Strand::Forward => "+",
            Strand::Reverse => "-",
            Strand::Unknown => "?",
        };
        let start = format!(
            "{}{}",
            if loc.partial_start { "<" } else { "" },
            loc.start() + 1
        );
        let end = format!("{}{}", if loc.partial_end { ">" } else { "" }, loc.end());
        let length: usize = loc.segments.iter().map(|s| s.len()).sum();
        TableRow::new([
            right(start),
            right(end),
            right(thousands(length)),
            Cell::from(strand),
            Cell::from(f.kind.as_key().to_string())
                .style(Style::default().fg(feature_color(&f.kind))),
            Cell::from(f.label().unwrap_or("-").to_string()),
            Cell::from(f.qualifier("product").unwrap_or("").to_string()),
        ])
    });

    let coord = record.len().to_string().len() as u16 + 1;
    let kind = features
        .iter()
        .map(|f| f.kind.as_key().len())
        .max()
        .unwrap_or(0)
        .max(6) as u16;
    let name = features
        .iter()
        .filter_map(|f| f.label())
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(6, 16) as u16;
    let widths = [
        Constraint::Length(coord.max(7)),
        Constraint::Length(coord.max(5)),
        Constraint::Length(8),
        Constraint::Length(1),
        Constraint::Length(kind),
        Constraint::Length(name),
        Constraint::Fill(1),
    ];

    let table = Table::new(table_rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::TOP).title(title.bold()))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .column_spacing(2);

    app.list_state.select(cursor);
    frame.render_stateful_widget(table, area, &mut app.list_state);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.mode {
        Mode::GoTo(input) => Line::from(vec![
            Span::raw(" Go to (position, START..END, or locus tag): ").bold(),
            Span::raw(format!("{input}█")),
        ]),
        Mode::Search(input) => Line::from(vec![
            Span::raw(" Search: ").bold(),
            Span::raw(format!("{input}█")),
        ]),
        _ => match &app.message {
            Some(message) => Line::from(format!(" {message}")).yellow(),
            None if app.list_open => Line::from(
                " ↑/↓ move  s sort  S reverse  / search  Enter go to feature  f/Esc close list",
            )
            .dim(),
            None => Line::from(
                " ←/→ pan  +/- zoom  Tab next feature  f feature list  / search  g go to  ? help  q quit",
            )
            .dim(),
        },
    };
    frame.render_widget(Paragraph::new(line), area);
}

const HELP: &[(&str, &str)] = &[
    ("← → / h l", "pan by a tenth of the view"),
    ("PgUp PgDn", "pan by most of the view"),
    ("Home End", "jump to start / end"),
    ("+ -", "zoom in / out"),
    ("0", "zoom out to the whole record"),
    ("Tab Shift-Tab", "select next / previous feature"),
    ("Enter", "zoom to the selected feature"),
    ("c", "GC skew track on / off (remembered)"),
    ("↑ ↓ / k j", "scroll the details panel"),
    ("/", "search locus tag, gene, product, protein ID"),
    ("n N", "next / previous match"),
    ("g", "go to position, START..END, or locus tag"),
    ("Backspace b", "back to the view before the last jump"),
    ("[ ]", "previous / next record"),
    ("Esc", "clear selection and search"),
    ("f", "open / close the feature list"),
    ("  ↑ ↓ PgUp PgDn", "  move through the list"),
    ("  s  S", "  change sort column / reverse order"),
    ("  Enter", "  close the list and zoom to the feature"),
    ("q", "quit"),
];

fn draw_help(frame: &mut Frame, area: Rect) {
    let key_width = HELP.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let mut lines: Vec<Line> = HELP
        .iter()
        .map(|(key, action)| {
            Line::from(vec![
                Span::styled(format!(" {key:<key_width$}  "), Style::default().bold()),
                Span::raw(*action),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(" Press any key to close").dim());

    let width = (lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16 + 4).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Keys ".bold())),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn fixture_app() -> App {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/CP000283_5k.gb");
        let records = crate::parser::parse_file(&path).unwrap();
        App::new(records, &|f| f.kind == FeatureKind::Cds, None)
    }

    #[test]
    fn renders_whole_record() {
        let mut app = fixture_app();
        let screen = render(&mut app, 120, 20);
        assert!(screen.contains("CP000283  5,000 bp"), "{screen}");
        assert!(screen.contains("1..5,000"), "{screen}");
        assert!(screen.contains("RPD_0001"), "{screen}");
        assert!(screen.contains("▶"), "{screen}");
        assert!(screen.contains("sequence shows at 1 bp/col"), "{screen}");
        assert_eq!(app.cols, 117);
    }

    #[test]
    fn renders_selection_details_and_sequence() {
        let mut app = fixture_app();
        render(&mut app, 120, 24);
        app.go_to("649..700");
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        // Tab zooms out to fit DnaA, so go back to base level at its start.
        app.go_to("649..700");
        let screen = render(&mut app, 120, 24);
        assert!(
            screen.contains("CDS  649..2067  1,419 bp  strand +"),
            "{screen}"
        );
        assert!(
            screen.contains("InterPro:IPR001957, InterPro:IPR003593"),
            "{screen}"
        );
        assert!(screen.contains("5' ATGTCGAATATG"), "{screen}");
        assert!(screen.contains("3' TACAGCTTATAC"), "{screen}");
    }

    #[test]
    fn labels_are_whole_or_absent() {
        assert_eq!(truncate("helicase-like protein", 10), "helicase-…");
        assert_eq!(truncate("short", 10), "short");

        let mut app = fixture_app();
        // ~93 bp/col on 117 columns: DnaA is ~15 columns wide.
        app.go_to("1..10,900");
        let screen = render(&mut app, 120, 20);
        assert!(!screen.contains("R…"), "{screen}");
        assert!(screen.contains("RPD_0001"), "{screen}");
    }

    #[test]
    fn renders_feature_list() {
        let mut app = fixture_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let screen = render(&mut app, 120, 40);
        assert!(
            screen.contains("Features (4) · sorted by position ▲"),
            "{screen}"
        );
        assert!(
            screen.contains("start ▲    end    length  ±  type"),
            "{screen}"
        );
        assert!(
            screen.contains("   2329   3447     1,119  +  CDS     RPD_0002  DNA polymerase III"),
            "{screen}"
        );
        assert!(screen.contains("   4985  >5000        16  +"), "{screen}");
        // The highlighted row is the selected feature.
        assert_eq!(app.list_state.selected(), Some(1));
        // Tall enough to keep the details panel under the list.
        assert!(
            screen.contains("CDS  2329..3447  1,119 bp  strand +"),
            "{screen}"
        );
        assert!(screen.contains("s sort"), "{screen}");

        // Short terminal: the list takes the whole panel; still no panic.
        let screen = render(&mut app, 120, 20);
        assert!(screen.contains("RPD_0002"), "{screen}");
        render(&mut app, 10, 4);
    }

    #[test]
    fn gc_skew_track_toggles() {
        let mut app = fixture_app();
        let screen = render(&mut app, 120, 24);
        let gc_line = screen
            .lines()
            .find(|l| l.starts_with("GC"))
            .unwrap_or_else(|| panic!("no GC track:\n{screen}"));
        assert!(gc_line.chars().any(|c| "▁▂▃▄▅▆▇█".contains(c)), "{gc_line}");
        assert!(
            screen.contains("Cumulative GC skew: minimum at"),
            "{screen}"
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        let screen = render(&mut app, 120, 24);
        assert!(!screen.lines().any(|l| l.starts_with("GC")), "{screen}");
    }

    #[test]
    fn gc_summary_wording() {
        let gc = GcCounts::new(b"CCCGGGGG");
        assert_eq!(
            gc_summary(&gc, Topology::Circular),
            "Cumulative GC skew: minimum at 3 (likely origin), maximum at 8 (likely terminus)"
        );
        assert_eq!(
            gc_summary(&GcCounts::new(b"ATAT"), Topology::Linear),
            "GC skew: no G or C in this sequence"
        );
    }

    #[test]
    fn renders_help_and_tiny_terminals() {
        let mut app = fixture_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        let screen = render(&mut app, 120, 30);
        assert!(screen.contains("Keys"), "{screen}");
        // Must not panic however small the terminal gets.
        for (w, h) in [(1, 1), (5, 3), (20, 5)] {
            render(&mut app, w, h);
        }
    }
}
