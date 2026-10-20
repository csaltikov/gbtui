//! Viewer state and key handling. Nothing here touches the terminal, so it
//! can be tested directly.

use std::ops::Range;
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use super::gc::{GcCounts, MIN_SKEW_LENGTH};
use super::tracks::pack_lanes;
use crate::model::{Feature, Record, Strand};
use crate::settings::Settings;

/// Width assumed until the first frame reports the real one.
const DEFAULT_COLS: usize = 100;
/// Widest view `g` leaves you at after going to a single position.
const GO_TO_SPAN: usize = 10_000;
/// Earlier views remembered for jumping back.
const HISTORY_LIMIT: usize = 50;

/// Where the viewer was: enough to come back to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ViewState {
    record: usize,
    start: usize,
    span: usize,
    selected: Option<usize>,
}

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
    /// GC counts for the skew track; `None` if the record has no sequence.
    pub gc: Option<GcCounts>,
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
            gc: (record.len() >= MIN_SKEW_LENGTH).then(|| GcCounts::new(&record.sequence)),
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

/// Column the feature list is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Position,
    Length,
    Type,
    Name,
    Product,
}

impl SortKey {
    fn next(self) -> Self {
        match self {
            SortKey::Position => SortKey::Length,
            SortKey::Length => SortKey::Type,
            SortKey::Type => SortKey::Name,
            SortKey::Name => SortKey::Product,
            SortKey::Product => SortKey::Position,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SortKey::Position => "position",
            SortKey::Length => "length",
            SortKey::Type => "type",
            SortKey::Name => "name",
            SortKey::Product => "product",
        }
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
    /// Whether the feature list replaces the details panel.
    pub list_open: bool,
    /// Scroll position of the list; its selection mirrors `selected`.
    pub list_state: TableState,
    /// Rows the list shows at once, set by the drawing code.
    pub list_page: usize,
    pub sort: SortKey,
    pub sort_reverse: bool,
    pub settings: Settings,
    /// Where to save settings when they change; `None` never saves.
    settings_path: Option<PathBuf>,
    /// Views before each jump, most recent last.
    history: Vec<ViewState>,
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
            list_open: false,
            list_state: TableState::default(),
            list_page: 10,
            sort: SortKey::Position,
            sort_reverse: false,
            settings: Settings::default(),
            settings_path: None,
            history: Vec::new(),
        };
        app.reset_view();
        app
    }

    /// Use remembered settings, saving changes back to `path`.
    pub fn with_settings(mut self, settings: Settings, path: Option<PathBuf>) -> Self {
        self.settings = settings;
        self.settings_path = path;
        self
    }

    fn toggle_gc_skew(&mut self) {
        // Leave the saved preference alone on records too short to show it.
        if self.layout().gc.is_none() {
            self.message = Some(format!(
                "GC skew isn't shown for sequences under {} kb; setting unchanged",
                MIN_SKEW_LENGTH / 1000
            ));
            return;
        }
        self.settings.gc_skew = !self.settings.gc_skew;
        let state = if self.settings.gc_skew { "on" } else { "off" };
        self.message = Some(match &self.settings_path {
            Some(path) => match self.settings.save(path) {
                Ok(()) => format!("GC skew track {state} (remembered)"),
                Err(e) => format!(
                    "GC skew track {state} (couldn't save {}: {e})",
                    path.display()
                ),
            },
            None => format!("GC skew track {state}"),
        });
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
        self.list_state = TableState::default();
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
            Mode::Normal if self.list_open => self.handle_list(key),
            Mode::Normal => self.handle_normal(key),
            Mode::Help => {}
            Mode::GoTo(input) => {
                if let Some(input) = edit_input(input, key, |s| self.jump(|app| app.go_to(&s))) {
                    self.mode = Mode::GoTo(input);
                }
            }
            Mode::Search(input) => {
                if let Some(input) = edit_input(input, key, |s| self.jump(|app| app.search_for(&s)))
                {
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
            KeyCode::Home => self.jump(|app| {
                app.start = 0;
                app.clamp();
            }),
            KeyCode::End => self.jump(|app| {
                app.start = usize::MAX;
                app.clamp();
            }),
            KeyCode::Char('+' | '=') => self.zoom_in(),
            KeyCode::Char('-' | '_') => self.zoom_out(),
            KeyCode::Char('0') => self.jump(|app| app.set_view(0..app.record().len())),
            KeyCode::Tab => self.next_feature(),
            KeyCode::BackTab => self.prev_feature(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_scroll = self.detail_scroll.saturating_add(1)
            }
            KeyCode::Char('/') => self.mode = Mode::Search(String::new()),
            KeyCode::Char('n') => self.jump(|app| app.cycle_match(true)),
            KeyCode::Char('N') => self.jump(|app| app.cycle_match(false)),
            KeyCode::Char('g') => self.mode = Mode::GoTo(String::new()),
            KeyCode::Char(']') => self.jump(|app| app.switch_record(1)),
            KeyCode::Char('[') => self.jump(|app| app.switch_record(-1)),
            KeyCode::Backspace | KeyCode::Char('b') => self.jump_back(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('f') => self.open_list(),
            KeyCode::Enter => self.jump(|app| app.zoom_to_selected()),
            KeyCode::Char('c') => self.toggle_gc_skew(),
            KeyCode::Char('y') => self.copy_selected_dna(),
            _ => {}
        }
    }

    /// Copy the selected feature's DNA (5'→3' on its strand) to the system
    /// clipboard, so it can be pasted outside the viewer.
    fn copy_selected_dna(&mut self) {
        let Some(feature) = self.selected_feature() else {
            self.message = Some("Nothing selected; press Tab or f to pick a feature".to_string());
            return;
        };
        let dna = feature.sequence(&self.record().sequence);
        if dna.is_empty() {
            self.message = Some("This feature has no sequence to copy".to_string());
            return;
        }
        let what = format!(
            "{} bp of {}",
            crate::thousands(dna.len()),
            feature.label().unwrap_or(feature.kind.as_key())
        );
        self.message = Some(match copy_to_clipboard(&dna) {
            Ok(()) => format!("Copied {what} to the clipboard"),
            Err(error) => format!("Couldn't copy {what}: {error}"),
        });
    }

    fn view_state(&self) -> ViewState {
        ViewState {
            record: self.current,
            start: self.start,
            span: self.span,
            selected: self.selected,
        }
    }

    /// Run a jump (go to, search, zoom to feature, ...) and remember where
    /// we were, if it actually moved anything.
    fn jump(&mut self, action: impl FnOnce(&mut Self)) {
        let before = self.view_state();
        action(self);
        if self.view_state() != before {
            if self.history.len() == HISTORY_LIMIT {
                self.history.remove(0);
            }
            self.history.push(before);
        }
    }

    /// Return to the view before the most recent jump.
    fn jump_back(&mut self) {
        let Some(state) = self.history.pop() else {
            self.message = Some("No earlier view to go back to".to_string());
            return;
        };
        if state.record != self.current {
            // Search matches belong to the record they were found in.
            self.search = None;
            self.list_state = TableState::default();
        }
        self.current = state.record;
        self.start = state.start;
        self.span = state.span;
        self.clamp();
        self.selected = state.selected;
        self.detail_scroll = 0;

        let window = self.window();
        let more = match self.history.len() {
            0 => String::new(),
            n => format!("  ({n} more)"),
        };
        self.message = Some(format!(
            "Back to {}..{}{more}",
            crate::thousands(window.start + 1),
            crate::thousands(window.end)
        ));
    }

    /// The search query, if a search is active.
    pub fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.query.as_str())
    }

    /// Rows of the feature list, as positions in `layout().shown`: the
    /// search matches if a search is active, otherwise every shown feature,
    /// in the current sort order.
    pub fn list_rows(&self) -> Vec<usize> {
        let mut rows: Vec<usize> = match &self.search {
            Some(search) => search.matches.clone(),
            None => (0..self.layout().shown.len()).collect(),
        };
        let record = self.record();
        let shown = &self.layout().shown;
        let feature = |k: usize| &record.features[shown[k]];
        let text = |k: usize, key: &str| {
            let f = feature(k);
            match key {
                "label" => f.label(),
                _ => f.qualifier(key),
            }
            .unwrap_or("")
            .to_lowercase()
        };

        // `shown` is already in position order, so a stable sort keeps
        // position as the tie-breaker for every other key.
        match self.sort {
            SortKey::Position => {}
            SortKey::Length => rows.sort_by_key(|&k| {
                let loc = &feature(k).location;
                std::cmp::Reverse(loc.end() - loc.start())
            }),
            SortKey::Type => {
                rows.sort_by(|&a, &b| feature(a).kind.as_key().cmp(feature(b).kind.as_key()))
            }
            SortKey::Name => rows.sort_by_cached_key(|&k| text(k, "label")),
            // Features without a product go last.
            SortKey::Product => rows.sort_by_cached_key(|&k| {
                let product = text(k, "product");
                (product.is_empty(), product)
            }),
        }
        if self.sort_reverse {
            rows.reverse();
        }
        rows
    }

    /// Row of the selected feature in `rows`, if it is listed.
    pub fn list_cursor(&self, rows: &[usize]) -> Option<usize> {
        self.selected
            .and_then(|k| rows.iter().position(|&r| r == k))
    }

    /// Whether feature `k` (a position in `shown`) overlaps the view.
    fn in_view(&self, k: usize) -> bool {
        self.record().features[self.layout().shown[k]]
            .location
            .overlaps(&self.window())
    }

    /// The selection, but only while it is on screen. A selection left
    /// behind after panning or `g` shouldn't decide where Tab or the list
    /// start.
    fn visible_selection(&self) -> Option<usize> {
        self.selected.filter(|&k| self.in_view(k))
    }

    fn open_list(&mut self) {
        self.list_open = true;
        self.follow_view();
    }

    /// Keep the selection in step with the view after it moves. A selection
    /// still on screen stays. Otherwise, with the list open, the cursor moves
    /// to the first listed feature in view (or the next one after it); with
    /// the list closed, the selection is cleared. The view itself never moves.
    fn follow_view(&mut self) {
        let rows = if self.list_open {
            self.list_rows()
        } else {
            Vec::new()
        };
        let keep = self
            .visible_selection()
            .filter(|k| !self.list_open || rows.contains(k));
        if keep.is_some() {
            return;
        }
        if !self.list_open {
            self.selected = None;
            return;
        }

        // Rows hold positions in `shown`, which is in start order, so the
        // smallest one starting after the view is the next feature along.
        let record = self.record();
        let shown = &self.layout().shown;
        let next_along = rows
            .iter()
            .copied()
            .filter(|&k| record.features[shown[k]].location.start() >= self.start)
            .min();
        let pick = rows
            .iter()
            .copied()
            .find(|&k| self.in_view(k))
            .or(next_along)
            .or_else(|| rows.iter().copied().max());
        self.selected = pick;
        self.detail_scroll = 0;
    }

    /// Select the first listed feature in view, or the first row when
    /// sorted by anything other than position.
    fn select_first_row(&mut self, rows: &[usize]) {
        let record = self.record();
        let shown = &self.layout().shown;
        let in_view = rows.iter().position(|&k| {
            let loc = &record.features[shown[k]].location;
            loc.end() > self.start
        });
        let row = match (self.sort, self.sort_reverse) {
            (SortKey::Position, false) => in_view.unwrap_or(0),
            _ => 0,
        };
        if let Some(&k) = rows.get(row) {
            self.select(k);
        }
    }

    /// Move the list cursor by `delta` rows, clamped to the list.
    fn move_cursor(&mut self, delta: isize) {
        let rows = self.list_rows();
        if rows.is_empty() {
            return;
        }
        let target = match self.list_cursor(&rows) {
            Some(row) => (row as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
            None => 0,
        };
        self.select(rows[target]);
    }

    fn handle_list(&mut self, key: KeyEvent) {
        self.message = None;
        let page = self.list_page.max(1) as isize;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => self.move_cursor(-1),
            KeyCode::PageDown => self.move_cursor(page),
            KeyCode::PageUp => self.move_cursor(-page),
            KeyCode::Home => self.move_cursor(isize::MIN / 2),
            KeyCode::End => self.move_cursor(isize::MAX / 2),
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.sort_reverse = false;
                self.select_first_row(&self.list_rows());
            }
            KeyCode::Char('S') => {
                self.sort_reverse = !self.sort_reverse;
                self.select_first_row(&self.list_rows());
            }
            KeyCode::Enter => {
                self.list_open = false;
                self.jump(|app| app.zoom_to_selected());
            }
            KeyCode::Char('f') => self.list_open = false,
            KeyCode::Esc if self.search.is_some() => {
                self.search = None;
                self.list_state = TableState::default();
            }
            KeyCode::Esc => self.list_open = false,
            // Everything else (pan, zoom, search, go to, help, quit) works
            // as usual while the list is open, and the list follows the view.
            _ => {
                self.handle_normal(key);
                if self.list_open {
                    self.follow_view();
                }
            }
        }
    }

    /// Zoom so the selected feature fills about two thirds of the view,
    /// or to 1 bp per column if it is smaller than that.
    fn zoom_to_selected(&mut self) {
        let Some(feature) = self.selected_feature() else {
            self.message = Some("Nothing selected; press Tab or f to pick a feature".to_string());
            return;
        };
        let (start, end) = (feature.location.start(), feature.location.end());
        let extent = end - start;
        self.span = extent + extent / 2;
        self.clamp();
        self.center_on(start + extent / 2);
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
        let next = match self.visible_selection() {
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
        let prev = match self.visible_selection() {
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
                // From a wide view the position may be impossible to centre
                // on (the window would run off the end), so zoom in first.
                self.span = self.span.min(GO_TO_SPAN);
                self.clamp();
                self.center_on(pos - 1);
                self.follow_view();
            }
            return;
        }

        if let Ok(region) = crate::parse_region(input) {
            if region.start >= len {
                self.message = Some(format!("Region starts past the end ({len} bp)"));
            } else {
                self.set_view(region.start..region.end.min(len));
                self.follow_view();
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

/// Put `text` on the system clipboard through the platform's clipboard
/// command, so no extra dependency is needed: `pbcopy` on macOS, then
/// `wl-copy`, `xclip` or `xsel` on Linux, `clip` on Windows.
fn copy_to_clipboard(text: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    const COMMANDS: &[&[&str]] = &[
        &["pbcopy"],
        &["wl-copy"],
        &["xclip", "-selection", "clipboard"],
        &["xsel", "--clipboard", "--input"],
        &["clip"],
    ];

    for command in COMMANDS {
        let Ok(mut child) = Command::new(command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue; // not installed; try the next one
        };
        let written = child
            .stdin
            .take()
            .ok_or_else(|| "no stdin".to_string())
            .and_then(|mut stdin| stdin.write_all(text).map_err(|e| e.to_string()));
        let status = child.wait().map_err(|e| e.to_string())?;
        return match written {
            Ok(()) if status.success() => Ok(()),
            Ok(()) => Err(format!("{} exited with {status}", command[0])),
            Err(error) => Err(format!("{}: {error}", command[0])),
        };
    }
    Err("no clipboard command found (pbcopy, wl-copy, xclip, xsel or clip)".to_string())
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

    /// A featureless record of `len` bases, for tests that need more room
    /// than the 5 kb fixture.
    fn synthetic_app(len: usize) -> App {
        use crate::model::{MoleculeType, Topology};
        let record = Record {
            name: "synthetic".to_string(),
            accession: None,
            version: None,
            definition: None,
            molecule: MoleculeType::Dna,
            topology: Topology::Circular,
            division: None,
            date: None,
            organism: None,
            features: Vec::new(),
            sequence: b"ACGT".repeat(len / 4),
        };
        let mut app = App::new(vec![record], &|_| true, None);
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

    fn listed_tags(app: &App) -> Vec<&str> {
        app.list_rows()
            .into_iter()
            .filter_map(|k| app.record().features[app.layout().shown[k]].qualifier("locus_tag"))
            .collect()
    }

    #[test]
    fn list_opens_on_first_feature_in_view() {
        let mut app = fixture_app(Some(2000..3000));
        press(&mut app, KeyCode::Char('f'));
        assert!(app.list_open);
        // RPD_0001 (649..2067) still reaches into the view.
        assert_eq!(selected_tag(&app), Some("RPD_0001"));
        press(&mut app, KeyCode::Down);
        assert_eq!(selected_tag(&app), Some("RPD_0002"));
        press(&mut app, KeyCode::End);
        assert_eq!(selected_tag(&app), Some("RPD_0004"));
        press(&mut app, KeyCode::Down);
        assert_eq!(selected_tag(&app), Some("RPD_0004"));
        press(&mut app, KeyCode::Home);
        assert_eq!(selected_tag(&app), Some("RPD_0001"));
    }

    #[test]
    fn list_sorts_and_reverses() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Length);
        // 1419, 1137, 1119, 16 bp
        assert_eq!(
            listed_tags(&app),
            ["RPD_0001", "RPD_0003", "RPD_0002", "RPD_0004"]
        );
        assert_eq!(selected_tag(&app), Some("RPD_0001"));

        press(&mut app, KeyCode::Char('S'));
        assert_eq!(
            listed_tags(&app),
            ["RPD_0004", "RPD_0002", "RPD_0003", "RPD_0001"]
        );
        assert_eq!(selected_tag(&app), Some("RPD_0004"));

        for _ in 0..3 {
            press(&mut app, KeyCode::Char('s'));
        }
        assert_eq!(app.sort, SortKey::Product);
        // chromosomal..., DNA gyrase, DNA polymerase, DNA replication
        assert_eq!(
            listed_tags(&app),
            ["RPD_0001", "RPD_0004", "RPD_0002", "RPD_0003"]
        );
    }

    #[test]
    fn list_shows_search_matches_and_esc_steps_back() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "dna ");
        assert!(app.list_open);
        assert_eq!(
            listed_tags(&app),
            ["RPD_0001", "RPD_0002", "RPD_0003", "RPD_0004"]
        );

        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "gyrase");
        assert_eq!(listed_tags(&app), ["RPD_0004"]);

        press(&mut app, KeyCode::Esc);
        assert!(app.list_open);
        assert_eq!(listed_tags(&app).len(), 4);
        press(&mut app, KeyCode::Esc);
        assert!(!app.list_open);
        assert_eq!(selected_tag(&app), Some("RPD_0004"));
    }

    #[test]
    fn enter_closes_list_and_zooms_to_feature() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Down);
        // Browsing only scrolls; the whole record is still in view.
        assert_eq!(app.window(), 0..5000);
        press(&mut app, KeyCode::Enter);
        assert!(!app.list_open);
        assert_eq!(selected_tag(&app), Some("RPD_0002"));
        // RPD_0002 is 2329..3447 (1,119 bp): view is 1.5x that, centred.
        assert_eq!(app.window(), 2048..3726);
        // Keys not used by the list still reach the viewer.
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Char('0'));
        assert_eq!(app.window(), 0..5000);
    }

    #[test]
    fn enter_zooms_small_features_to_base_level() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "RPD_0004");
        press(&mut app, KeyCode::Enter);
        // 16 bp feature at the end of the record: 1 bp per column, clamped.
        assert_eq!(app.window(), 4900..5000);
    }

    #[test]
    fn enter_without_selection_explains() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.window(), 0..5000);
        assert!(app.message.as_deref().unwrap().contains("Nothing selected"));
    }

    #[test]
    fn c_toggles_gc_skew_and_saves_it() {
        let dir = std::env::temp_dir().join(format!("gbtui-app-{}", std::process::id()));
        let path = dir.join("settings.conf");
        let mut app = synthetic_app(100_000).with_settings(Settings::default(), Some(path.clone()));
        assert!(app.settings.gc_skew);

        press(&mut app, KeyCode::Char('c'));
        assert!(!app.settings.gc_skew);
        assert_eq!(
            app.message.as_deref(),
            Some("GC skew track off (remembered)")
        );
        assert_eq!(Settings::load(&path), Settings { gc_skew: false });

        // Works from the feature list too.
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(Settings::load(&path), Settings { gc_skew: true });

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn toggle_without_a_settings_path_does_not_save() {
        let mut app = synthetic_app(100_000);
        press(&mut app, KeyCode::Char('c'));
        assert!(!app.settings.gc_skew);
        assert_eq!(app.message.as_deref(), Some("GC skew track off"));
    }

    #[test]
    fn list_opens_in_the_current_view_after_go_to() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Tab);
        assert_eq!(selected_tag(&app), Some("RPD_0001"));

        // Jump away: the old selection is now off screen.
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "4900..5000");
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(selected_tag(&app), Some("RPD_0004"));
        assert_eq!(app.window(), 4899..5000);
    }

    #[test]
    fn list_opens_in_view_whatever_the_sort() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.sort, SortKey::Length);
        press(&mut app, KeyCode::Char('f'));

        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "3000..3100");
        press(&mut app, KeyCode::Char('f'));
        // Only RPD_0002 (2329..3447) is in view; the longest feature isn't.
        // It's selected where it is, without zooming out to fit it.
        assert_eq!(selected_tag(&app), Some("RPD_0002"));
        assert_eq!(app.window(), 2999..3100);
    }

    #[test]
    fn list_keeps_a_visible_selection() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        assert_eq!(selected_tag(&app), Some("RPD_0002"));
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(selected_tag(&app), Some("RPD_0002"));
    }

    #[test]
    fn tab_starts_from_the_view_after_go_to() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Tab);
        assert_eq!(selected_tag(&app), Some("RPD_0001"));

        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "3600..3700");
        press(&mut app, KeyCode::Tab);
        assert_eq!(selected_tag(&app), Some("RPD_0003"));

        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "4900..5000");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(selected_tag(&app), Some("RPD_0004"));
    }

    #[test]
    fn go_to_with_list_open_moves_the_list_to_the_new_window() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(selected_tag(&app), Some("RPD_0001"));

        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "3000..3100");
        assert!(app.list_open);
        assert_eq!(app.window(), 2999..3100);
        assert_eq!(selected_tag(&app), Some("RPD_0002"));

        // A gap between genes: the cursor goes to the next feature along,
        // and the view stays where `g` put it.
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "3450..3550");
        assert_eq!(app.window(), 3449..3550);
        assert_eq!(selected_tag(&app), Some("RPD_0003"));

        // By name still selects that feature.
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "RPD_0001");
        assert_eq!(selected_tag(&app), Some("RPD_0001"));
        assert!(app.list_open);
    }

    #[test]
    fn list_follows_panning() {
        let mut app = fixture_app(Some(0..1000));
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(selected_tag(&app), Some("RPD_0001"));
        // (Home/End move within the list, so pan with the arrow keys.)
        for _ in 0..25 {
            press(&mut app, KeyCode::Right);
        }
        assert_eq!(app.window(), 2500..3500);
        assert_eq!(selected_tag(&app), Some("RPD_0002"));
        for _ in 0..15 {
            press(&mut app, KeyCode::Right);
        }
        assert_eq!(app.window(), 4000..5000);
        // RPD_0003 (3622..4758) is the first feature reaching into view.
        assert_eq!(selected_tag(&app), Some("RPD_0003"));
    }

    #[test]
    fn go_to_with_list_closed_drops_an_off_screen_selection() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Tab);
        assert_eq!(selected_tag(&app), Some("RPD_0001"));

        // Still on screen: kept.
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "1000..1500");
        assert_eq!(selected_tag(&app), Some("RPD_0001"));

        // Gone: cleared, so the details panel shows the record again.
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "3000..3100");
        assert_eq!(app.selected, None);

        // Panning with the list closed keeps the selection.
        press(&mut app, KeyCode::Tab);
        assert_eq!(selected_tag(&app), Some("RPD_0003"));
        press(&mut app, KeyCode::Home);
        assert_eq!(selected_tag(&app), Some("RPD_0003"));
    }

    #[test]
    fn go_to_from_whole_record_zooms_in_and_centres() {
        let mut app = synthetic_app(100_000);
        assert_eq!(app.window(), 0..100_000);

        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "70,000");
        // 10 kb centred on base 70,000 (69,999 0-based).
        assert_eq!(app.window(), 64_999..74_999);

        // + keeps zooming in on the same spot.
        press(&mut app, KeyCode::Char('+'));
        assert_eq!(app.window(), 67_499..72_499);
        press(&mut app, KeyCode::Char('+'));
        let w = app.window();
        assert!(w.contains(&69_999) && w.len() == 2_500, "{w:?}");
    }

    #[test]
    fn go_to_keeps_a_closer_zoom() {
        let mut app = synthetic_app(100_000);
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "50,001..51,000");
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "20,000");
        assert_eq!(app.window(), 19_499..20_499);
    }

    #[test]
    fn go_to_near_the_ends_still_shows_the_position() {
        let mut app = synthetic_app(100_000);
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "99,990");
        assert_eq!(app.window(), 90_000..100_000);
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "5");
        assert_eq!(app.window(), 0..10_000);
    }

    #[test]
    fn backspace_walks_back_through_jumps() {
        let mut app = synthetic_app(100_000);
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "70,000");
        press(&mut app, KeyCode::Char('+'));
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "20,000");
        assert_eq!(app.window(), 17_499..22_499);

        // Back to where the second `g` started, zoom included.
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.window(), 67_499..72_499);
        assert_eq!(
            app.message.as_deref(),
            Some("Back to 67,500..72,499  (1 more)")
        );

        press(&mut app, KeyCode::Char('b'));
        assert_eq!(app.window(), 0..100_000);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.window(), 0..100_000);
        assert_eq!(
            app.message.as_deref(),
            Some("No earlier view to go back to")
        );
    }

    #[test]
    fn pans_and_failed_jumps_are_not_remembered() {
        let mut app = synthetic_app(100_000);
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "50,000");
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Char('-'));
        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "not-a-place");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.window(), 0..100_000);
        assert!(app.history.is_empty());
    }

    #[test]
    fn back_restores_selection_after_search_and_zoom() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "gyrase");
        assert_eq!(selected_tag(&app), Some("RPD_0004"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.window(), 4900..5000);

        press(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "1000..1200");
        // The off-screen selection was dropped by `g`...
        assert_eq!(app.selected, None);

        // ...and comes back with the view.
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.window(), 4900..5000);
        assert_eq!(selected_tag(&app), Some("RPD_0004"));
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.window(), 0..5000);
        assert_eq!(selected_tag(&app), Some("RPD_0004"));
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.selected, None);
    }

    #[test]
    fn back_works_from_the_feature_list() {
        let mut app = fixture_app(None);
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.window(), 2048..3726);
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Backspace);
        assert!(app.list_open);
        assert_eq!(app.window(), 0..5000);
    }

    #[test]
    fn history_is_capped() {
        let mut app = synthetic_app(100_000);
        for i in 0..60 {
            press(&mut app, KeyCode::Char('g'));
            type_text(&mut app, &format!("{}", 1000 + i * 1000));
        }
        assert_eq!(app.history.len(), HISTORY_LIMIT);
    }

    #[test]
    fn c_on_a_short_record_explains_and_keeps_the_setting() {
        let dir = std::env::temp_dir().join(format!("gbtui-short-{}", std::process::id()));
        let path = dir.join("settings.conf");
        // The fixture is 5 kb, under the cut-off.
        let mut app = fixture_app(None).with_settings(Settings::default(), Some(path.clone()));
        assert!(app.layout().gc.is_none());
        press(&mut app, KeyCode::Char('c'));
        assert!(app.settings.gc_skew);
        assert_eq!(
            app.message.as_deref(),
            Some("GC skew isn't shown for sequences under 20 kb; setting unchanged")
        );
        assert!(!path.exists());
    }

    #[test]
    fn gc_skew_cut_off_is_20_kb() {
        assert!(synthetic_app(20_000).layout().gc.is_some());
        assert!(synthetic_app(19_996).layout().gc.is_none());
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
