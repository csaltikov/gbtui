//! Viewer state and key handling. Nothing here touches the terminal, so it
//! can be tested directly.

use std::ops::Range;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::tracks::pack_lanes;
use crate::model::{Feature, Record, Strand};

/// Width assumed until the first frame reports the real one.
const DEFAULT_COLS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    GoTo(String),
    Search(String),
    Help,
}

/// Which track a feature is drawn on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    Forward,
    Reverse,
}

/// Per-record drawing data, computed once.
pub struct Layout {
    /// Indices into `record.features` of the features drawn, sorted by start.
    pub shown: Vec<usize>,
    /// Track and lane for each entry of `shown`.
    pub placement: Vec<(Track, usize)>,
    pub forward_lanes: usize,
    pub reverse_lanes: usize,
    /// Longest feature extent, so window lookups can binary-search on start.
    max_extent: usize,
}

impl Layout {
    fn new(record: &Record, filter: &dyn Fn(&Feature) -> bool) -> Self {
        let mut shown: Vec<usize> = (0..record.features.len())
            .filter(|&i| filter(&record.features[i]))
            .collect();
        shown.sort_by_key(|&i| {
            let loc = &record.features[i].location;
            (loc.start(), loc.end())
        });

        let track_of = |i: usize| match record.features[i].location.strand {
            Strand::Reverse => Track::Reverse,
            Strand::Forward | Strand::Unknown => Track::Forward,
        };

        // Pack each track separately; `lane_by_feature` is indexed like
        // `record.features`.
        let mut lane_by_feature = vec![0; record.features.len()];
        let mut pack = |track: Track| {
            let members: Vec<usize> = shown
                .iter()
                .copied()
                .filter(|&i| track_of(i) == track)
                .collect();
            let (lanes, count) = pack_lanes(members.iter().map(|&i| {
                let loc = &record.features[i].location;
                (loc.start(), loc.end())
            }));
            for (i, lane) in members.into_iter().zip(lanes) {
                lane_by_feature[i] = lane;
            }
            count
        };
        let forward_lanes = pack(Track::Forward);
        let reverse_lanes = pack(Track::Reverse);
        let placement = shown
            .iter()
            .map(|&i| (track_of(i), lane_by_feature[i]))
            .collect();

        let max_extent = shown
            .iter()
            .map(|&i| {
                let loc = &record.features[i].location;
                loc.end() - loc.start()
            })
            .max()
            .unwrap_or(0);

        Layout {
            shown,
            placement,
            forward_lanes,
            reverse_lanes,
            max_extent,
        }
    }

    /// Positions in `shown` of features overlapping `window`.
    pub fn in_window<'a>(
        &'a self,
        record: &'a Record,
        window: Range<usize>,
    ) -> impl Iterator<Item = usize> + 'a {
        // Features are sorted by start, so only those starting within
        // `max_extent` before the window can reach into it.
        let first = self.shown.partition_point(|&i| {
            record.features[i].location.start() + self.max_extent < window.start
        });
        let last = self
            .shown
            .partition_point(|&i| record.features[i].location.start() < window.end);
        (first..last).filter(move |&k| record.features[self.shown[k]].location.overlaps(&window))
    }
}

struct Search {
    query: String,
    /// Positions in `shown`.
    matches: Vec<usize>,
    current: usize,
}

pub struct App {
    pub records: Vec<Record>,
    layouts: Vec<Layout>,
    pub current: usize,
    /// First base in view (0-based).
    pub start: usize,
    /// Bases in view.
    pub span: usize,
    /// Columns available for tracks, set by the drawing code.
    pub cols: usize,
    /// Selected feature, as a position in `layout().shown`.
    pub selected: Option<usize>,
    pub mode: Mode,
    pub message: Option<String>,
    search: Option<Search>,
    pub detail_scroll: u16,
    pub quit: bool,
    initial_region: Option<Range<usize>>,
}

impl App {
    /// `records` must not be empty. `region` is 0-based half-open.
    pub fn new(
        records: Vec<Record>,
        filter: &dyn Fn(&Feature) -> bool,
        region: Option<Range<usize>>,
    ) -> Self {
        assert!(!records.is_empty(), "App needs at least one record");
        let layouts = records.iter().map(|r| Layout::new(r, filter)).collect();
        let mut app = App {
            records,
            layouts,
            current: 0,
            start: 0,
            span: 0,
            cols: DEFAULT_COLS,
            selected: None,
            mode: Mode::Normal,
            message: None,
            search: None,
            detail_scroll: 0,
            quit: false,
            initial_region: region,
        };
        app.reset_view();
        app
    }

    pub fn record(&self) -> &Record {
        &self.records[self.current]
    }

    pub fn layout(&self) -> &Layout {
        &self.layouts[self.current]
    }

    pub fn selected_feature(&self) -> Option<&Feature> {
        self.selected
            .map(|k| &self.record().features[self.layout().shown[k]])
    }

    pub fn is_match(&self, k: usize) -> bool {
        self.search.as_ref().is_some_and(|s| s.matches.contains(&k))
    }

    pub fn bp_per_col(&self) -> f64 {
        self.span as f64 / self.cols as f64
    }

    pub fn window(&self) -> Range<usize> {
        self.start..self.start + self.span
    }

    pub fn set_cols(&mut self, cols: usize) {
        self.cols = cols.max(1);
        self.clamp();
    }

    /// Keep the view inside the record, at no more than one base per column.
    fn clamp(&mut self) {
        let len = self.record().len().max(1);
        let min_span = self.cols.min(len);
        self.span = self.span.clamp(min_span, len);
        self.start = self.start.min(len - self.span);
    }

    fn set_view(&mut self, range: Range<usize>) {
        self.start = range.start;
        self.span = range.len();
        self.clamp();
    }

    fn center_on(&mut self, pos: usize) {
        self.start = pos.saturating_sub(self.span / 2);
        self.clamp();
    }

    fn reset_view(&mut self) {
        let len = self.record().len();
        match &self.initial_region {
            Some(r) if r.start < len => self.set_view(r.start..r.end.min(len)),
            _ => self.set_view(0..len),
        }
        self.selected = None;
        self.search = None;
        self.detail_scroll = 0;
    }

    fn pan(&mut self, fraction: f64) {
        let delta = ((self.span as f64 * fraction.abs()).round() as usize).max(1);
        self.start = if fraction < 0.0 {
            self.start.saturating_sub(delta)
        } else {
            self.start.saturating_add(delta)
        };
        self.clamp();
    }

    fn zoom_in(&mut self) {
        let center = self.start + self.span / 2;
        self.span /= 2;
        self.center_on(center);
    }

    fn zoom_out(&mut self) {
        let center = self.start + self.span / 2;
        self.span = self.span.saturating_mul(2);
        self.center_on(center);
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.handle_normal(key),
            Mode::Help => {}
            Mode::GoTo(input) => {
                if let Some(input) = edit_input(input, key, |s| self.go_to(&s)) {
                    self.mode = Mode::GoTo(input);
                }
            }
            Mode::Search(input) => {
                if let Some(input) = edit_input(input, key, |s| self.search_for(&s)) {
                    self.mode = Mode::Search(input);
                }
            }
        }
    }

    fn handle_normal(&mut self, key: KeyEvent) {
        self.message = None;
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                self.selected = None;
                self.search = None;
            }
            KeyCode::Left | KeyCode::Char('h') => self.pan(-0.1),
            KeyCode::Right | KeyCode::Char('l') => self.pan(0.1),
            KeyCode::PageUp => self.pan(-0.9),
            KeyCode::PageDown => self.pan(0.9),
            KeyCode::Home => {
                self.start = 0;
                self.clamp();
            }
            KeyCode::End => {
                self.start = usize::MAX;
                self.clamp();
            }
            KeyCode::Char('+' | '=') => self.zoom_in(),
            KeyCode::Char('-' | '_') => self.zoom_out(),
            KeyCode::Char('0') => self.set_view(0..self.record().len()),
            KeyCode::Tab => self.next_feature(),
            KeyCode::BackTab => self.prev_feature(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_scroll = self.detail_scroll.saturating_add(1)
            }
            KeyCode::Char('/') => self.mode = Mode::Search(String::new()),
            KeyCode::Char('n') => self.cycle_match(true),
            KeyCode::Char('N') => self.cycle_match(false),
            KeyCode::Char('g') => self.mode = Mode::GoTo(String::new()),
            KeyCode::Char(']') => self.switch_record(1),
            KeyCode::Char('[') => self.switch_record(-1),
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
    }

    fn select(&mut self, k: usize) {
        self.selected = Some(k);
        self.detail_scroll = 0;

        let loc = &self.record().features[self.layout().shown[k]].location;
        let (start, end) = (loc.start(), loc.end());
        let extent = end - start;
        if extent > self.span {
            self.span = extent + extent / 5;
        }
        if start < self.start || end > self.start + self.span {
            self.center_on(start + extent / 2);
        }
        self.clamp();
    }

    fn next_feature(&mut self) {
        let shown = &self.layout().shown;
        let next = match self.selected {
            Some(k) if k + 1 < shown.len() => Some(k + 1),
            Some(_) => None,
            None => {
                let record = self.record();
                let k =
                    shown.partition_point(|&i| record.features[i].location.start() < self.start);
                (k < shown.len()).then_some(k)
            }
        };
        match next {
            Some(k) => self.select(k),
            None => self.message = Some("No more features".to_string()),
        }
    }

    fn prev_feature(&mut self) {
        let prev = match self.selected {
            Some(k) => k.checked_sub(1),
            None => {
                let record = self.record();
                let end = self.start + self.span;
                let k = self
                    .layout()
                    .shown
                    .partition_point(|&i| record.features[i].location.start() < end);
                k.checked_sub(1)
            }
        };
        match prev {
            Some(k) => self.select(k),
            None => self.message = Some("No earlier features".to_string()),
        }
    }

    fn switch_record(&mut self, step: isize) {
        let target = self.current as isize + step;
        if target < 0 || target as usize >= self.records.len() {
            self.message = Some(format!("{} record(s) in this file", self.records.len()));
            return;
        }
        self.current = target as usize;
        self.reset_view();
    }

    /// Jump to a 1-based position, a `START..END` region, or a feature by
    /// locus tag or gene name.
    pub fn go_to(&mut self, input: &str) {
        let input = input.trim();
        if input.is_empty() {
            return;
        }
        let len = self.record().len();

        if let Ok(pos) = input.replace(',', "").parse::<usize>() {
            if pos == 0 || pos > len {
                self.message = Some(format!("Position must be between 1 and {len}"));
            } else {
                self.center_on(pos - 1);
            }
            return;
        }

        if let Ok(region) = crate::parse_region(input) {
            if region.start >= len {
                self.message = Some(format!("Region starts past the end ({len} bp)"));
            } else {
                self.set_view(region.start..region.end.min(len));
            }
            return;
        }

        let record = self.record();
        let found = self.layout().shown.iter().position(|&i| {
            let f = &record.features[i];
            ["locus_tag", "gene", "protein_id"]
                .iter()
                .filter_map(|key| f.qualifier(key))
                .any(|v| v.eq_ignore_ascii_case(input))
        });
        match found {
            Some(k) => self.select(k),
            None => self.message = Some(format!("Not a position, region or feature name: {input}")),
        }
    }

    /// Find features whose locus tag, gene, product or protein ID contains
    /// `query` (case-insensitive), and select the first one from the view on.
    pub fn search_for(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            self.search = None;
            return;
        }
        let needle = query.to_lowercase();
        let record = self.record();
        let matches: Vec<usize> = self
            .layout()
            .shown
            .iter()
            .enumerate()
            .filter(|&(_, &i)| {
                let f = &record.features[i];
                ["locus_tag", "gene", "product", "protein_id"]
                    .iter()
                    .filter_map(|key| f.qualifier(key))
                    .any(|v| v.to_lowercase().contains(&needle))
            })
            .map(|(k, _)| k)
            .collect();

        if matches.is_empty() {
            self.search = None;
            self.message = Some(format!("No matches for \"{query}\""));
            return;
        }

        let current = matches
            .iter()
            .position(|&k| record.features[self.layout().shown[k]].location.start() >= self.start)
            .unwrap_or(0);
        let k = matches[current];
        self.message = Some(format!(
            "Match {}/{} for \"{query}\"",
            current + 1,
            matches.len()
        ));
        self.search = Some(Search {
            query: query.to_string(),
            matches,
            current,
        });
        self.select(k);
    }

    fn cycle_match(&mut self, forward: bool) {
        let Some(search) = &mut self.search else {
            self.message = Some("No search; press / to search".to_string());
            return;
        };
        let n = search.matches.len();
        search.current = if forward {
            (search.current + 1) % n
        } else {
            (search.current + n - 1) % n
        };
        let k = search.matches[search.current];
        self.message = Some(format!(
            "Match {}/{} for \"{}\"",
            search.current + 1,
            n,
            search.query
        ));
        self.select(k);
    }
}

/// Apply a key to a text prompt. Returns the updated text while still
/// editing, or `None` once the prompt is submitted (Enter) or cancelled (Esc).
fn edit_input(mut input: String, key: KeyEvent, submit: impl FnOnce(String)) -> Option<String> {
    match key.code {
        KeyCode::Enter => {
            submit(input);
            None
        }
        KeyCode::Esc => None,
        KeyCode::Backspace => {
            input.pop();
            Some(input)
        }
        KeyCode::Char(c) => {
            input.push(c);
            Some(input)
        }
        _ => Some(input),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::FeatureKind;

    fn fixture_app(region: Option<Range<usize>>) -> App {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/CP000283_5k.gb");
        let records = crate::parser::parse_file(&path).unwrap();
        let mut app = App::new(records, &|f| f.kind == FeatureKind::Cds, region);
        app.set_cols(100);
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
        press(app, KeyCode::Enter);
    }

    fn selected_tag(app: &App) -> Option<&str> {
        app.selected_feature()
            .and_then(|f| f.qualifier("locus_tag"))
    }

    #[test]
    fn starts_zoomed_out_or_at_region() {
        let app = fixture_app(None);
        assert_eq!(app.window(), 0..5000);

        let app = fixture_app(Some(999..2000));
        assert_eq!(app.window(), 999..2000);
    }

    #[test]
    fn zoom_is_limited_to_one_base_per_column() {
        let mut app = fixture_app(None);
        for _ in 0..20 {
            press(&mut app, KeyCode::Char('+'));
        }
        assert_eq!(app.span, 100);
        for _ in 0..20 {
            press(&mut app, KeyCode::Char('-'));
        }
        assert_eq!(app.window(), 0..5000);
    }

    #[test]
    fn pan_stays_inside_record() {
        let mut app = fixture_app(Some(0..1000));
        press(&mut app, KeyCode::Left);
        assert_eq!(app.start, 0);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.start, 100);
        press(&mut app, KeyCode::End);
        assert_eq!(app.window(), 4000..5000);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.window(), 4000..5000);
    }

    #[test]
    fn tab_steps_through_features_in_order() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Tab);
        assert_eq!(selected_tag(&app), Some("RPD_0001"));
        press(&mut app, KeyCode::Tab);
        assert_eq!(selected_tag(&app), Some("RPD_0002"));
        press(&mut app, KeyCode::BackTab);
        assert_eq!(selected_tag(&app), Some("RPD_0001"));
        press(&mut app, KeyCode::BackTab);
        assert_eq!(selected_tag(&app), Some("RPD_0001"));
        assert!(app.message.is_some());
    }

    #[test]
    fn selecting_scrolls_feature_into_view() {
        let mut app = fixture_app(Some(0..200));
        press(&mut app, KeyCode::Tab);
        let window = app.window();
        let loc = &app.selected_feature().unwrap().location;
        assert!(window.start <= loc.start() && loc.end() <= window.end);
    }

    #[test]
    fn go_to_position_region_and_locus_tag() {
        let mut app = fixture_app(Some(0..1000));
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "3,000");
        assert_eq!(app.window(), 2499..3499);

        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "100..600");
        assert_eq!(app.window(), 99..600);

        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "rpd_0003");
        assert_eq!(selected_tag(&app), Some("RPD_0003"));

        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "nope");
        assert!(app.message.as_deref().unwrap().contains("nope"));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn search_and_cycle_matches() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "dna");
        // DnaA, DNA polymerase III, DNA replication ... RecF, DNA gyrase
        assert_eq!(selected_tag(&app), Some("RPD_0001"));
        assert_eq!(app.message.as_deref(), Some("Match 1/4 for \"dna\""));
        assert!(app.is_match(0));

        press(&mut app, KeyCode::Char('N'));
        assert_eq!(selected_tag(&app), Some("RPD_0004"));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(selected_tag(&app), Some("RPD_0001"));

        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "zzz");
        assert_eq!(app.message.as_deref(), Some("No matches for \"zzz\""));
    }

    #[test]
    fn esc_cancels_prompt() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('1'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.window(), 0..5000);
    }

    #[test]
    fn in_window_finds_overlapping_features() {
        let app = fixture_app(None);
        let tags: Vec<&str> = app
            .layout()
            .in_window(app.record(), 2000..2400)
            .filter_map(|k| app.record().features[app.layout().shown[k]].qualifier("locus_tag"))
            .collect();
        assert_eq!(tags, ["RPD_0001", "RPD_0002"]);
    }
}
