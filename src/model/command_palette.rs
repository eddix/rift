//! Pure command-palette state, ranking, and navigation.

use rift_protocol::LayoutMode;

use crate::actor::app::WindowId;
use crate::common::collections::HashMap;
use crate::sys::window_server::WindowServerId;

const CURRENT_WORKSPACE_BOOST: i64 = 400;
const CURRENT_DISPLAY_BOOST: i64 = 200;
const MRU_BASE_SCORE: i64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PaletteEntryId {
    Window(WindowId),
    Application(i32),
    Command(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteEntryKind {
    Window,
    Application,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    Windows,
    Commands,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    FocusWindow {
        window_id: WindowId,
        window_server_id: Option<WindowServerId>,
    },
    ActivateApplication(i32),
    SwitchWorkspace(usize),
    MoveWindowToWorkspace(usize),
    NextWorkspace,
    PreviousWorkspace,
    LastWorkspace,
    ToggleFloating,
    ToggleFullscreen,
    ToggleFullscreenWithinGaps,
    SetLayout(LayoutMode),
    FocusDisplay(String),
    ReloadConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteEntry {
    pub id: PaletteEntryId,
    pub kind: PaletteEntryKind,
    pub primary: String,
    pub secondary: String,
    pub app_pid: Option<i32>,
    pub action: PaletteAction,
    pub show_when_empty: bool,
    pub is_current_workspace: bool,
    pub is_current_display: bool,
    search_penalty: i64,
    search_terms: Vec<String>,
}

impl PaletteEntry {
    pub fn new(
        id: PaletteEntryId,
        kind: PaletteEntryKind,
        primary: String,
        secondary: String,
        keywords: impl IntoIterator<Item = String>,
        app_pid: Option<i32>,
        action: PaletteAction,
    ) -> Self {
        let mut search_terms = vec![normalize(&primary), normalize(&secondary)];
        search_terms.extend(keywords.into_iter().map(|keyword| normalize(&keyword)));
        search_terms.retain(|term| !term.is_empty());
        Self {
            id,
            kind,
            primary,
            secondary,
            app_pid,
            action,
            show_when_empty: true,
            is_current_workspace: false,
            is_current_display: false,
            search_penalty: 0,
            search_terms,
        }
    }

    pub fn hidden_when_empty(mut self) -> Self {
        self.show_when_empty = false;
        self
    }

    pub fn with_location_boosts(mut self, current_workspace: bool, current_display: bool) -> Self {
        self.is_current_workspace = current_workspace;
        self.is_current_display = current_display;
        self
    }

    pub fn with_search_penalty(mut self, penalty: i64) -> Self {
        self.search_penalty = penalty.max(0);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteFocusOrigin {
    pub app_pid: i32,
    pub window_id: Option<WindowId>,
    pub window_server_id: Option<WindowServerId>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PaletteSnapshot {
    pub entries: Vec<PaletteEntry>,
    pub focus_origin: Option<PaletteFocusOrigin>,
    pub target_display_id: Option<u32>,
}

#[derive(Debug, Default)]
pub struct PaletteMru {
    sequence: u64,
    windows: HashMap<WindowId, u64>,
    applications: HashMap<i32, u64>,
}

impl PaletteMru {
    pub fn record_window(&mut self, window: WindowId) {
        self.sequence = self.sequence.saturating_add(1);
        self.windows.insert(window, self.sequence);
        self.applications.insert(window.pid, self.sequence);
    }

    fn score(&self, entry: &PaletteEntry) -> i64 {
        let sequence = match entry.id {
            PaletteEntryId::Window(window) => self.windows.get(&window).copied(),
            PaletteEntryId::Application(pid) => self.applications.get(&pid).copied(),
            PaletteEntryId::Command(_) => None,
        };
        sequence
            .and_then(|sequence| i64::try_from(sequence).ok())
            .map_or(0, |sequence| MRU_BASE_SCORE.saturating_add(sequence))
    }

    fn window_sequence(&self, window: WindowId) -> u64 {
        self.windows.get(&window).copied().unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PaletteScope {
    Root,
    Commands,
    Application { pid: i32, previous_query: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RankedEntry {
    index: usize,
    score: i64,
}

#[derive(Debug)]
pub struct PaletteModel {
    snapshot: PaletteSnapshot,
    query: String,
    normalized_query: String,
    scope: PaletteScope,
    results: Vec<RankedEntry>,
    selection: Option<usize>,
}

impl PaletteModel {
    pub fn new() -> Self {
        Self {
            snapshot: PaletteSnapshot::default(),
            query: String::new(),
            normalized_query: String::new(),
            scope: PaletteScope::Root,
            results: Vec::new(),
            selection: None,
        }
    }

    pub fn standard() -> Self { Self::new() }

    pub fn begin_session(
        &mut self,
        snapshot: PaletteSnapshot,
        mru: &PaletteMru,
        mode: PaletteMode,
    ) {
        self.snapshot = snapshot;
        self.query.clear();
        self.normalized_query.clear();
        self.scope = match mode {
            PaletteMode::Windows => PaletteScope::Root,
            PaletteMode::Commands => PaletteScope::Commands,
        };
        self.rebuild(mru, None);
    }

    pub fn set_snapshot(&mut self, snapshot: PaletteSnapshot, mru: &PaletteMru) {
        let selected = self.selected_entry().map(|entry| entry.id.clone());
        self.snapshot = snapshot;
        self.rebuild(mru, selected.as_ref());
    }

    pub fn set_query(&mut self, query: String, mru: &PaletteMru) {
        self.normalized_query = normalize(&query);
        self.query = query;
        self.rebuild(mru, None);
    }

    pub fn query(&self) -> &str { &self.query }

    pub fn results(&self) -> impl Iterator<Item = &PaletteEntry> {
        self.results.iter().map(|ranked| &self.snapshot.entries[ranked.index])
    }

    pub fn selected_entry(&self) -> Option<&PaletteEntry> {
        let result = self.results.get(self.selection?)?;
        self.snapshot.entries.get(result.index)
    }

    pub fn selected_index(&self) -> Option<usize> { self.selection }

    pub fn select_index(&mut self, index: usize) -> bool {
        if index >= self.results.len() {
            return false;
        }
        self.selection = Some(index);
        true
    }

    pub fn preferred_window_for_application(
        &self,
        pid: i32,
        mru: &PaletteMru,
    ) -> Option<PaletteAction> {
        self.snapshot
            .entries
            .iter()
            .filter(|entry| entry.kind == PaletteEntryKind::Window && entry.app_pid == Some(pid))
            .max_by_key(|entry| match entry.id {
                PaletteEntryId::Window(window) => (
                    mru.window_sequence(window),
                    entry.is_current_workspace,
                    entry.is_current_display,
                ),
                _ => (0, false, false),
            })
            .map(|entry| entry.action.clone())
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            self.selection = None;
            return;
        }
        let len = self.results.len() as isize;
        let current = self.selection.unwrap_or(0) as isize;
        self.selection = Some((current + delta).rem_euclid(len) as usize);
    }

    pub fn expand_selected_application(&mut self, mru: &PaletteMru) -> bool {
        let Some(entry) = self.selected_entry() else {
            return false;
        };
        if entry.kind != PaletteEntryKind::Application {
            return false;
        }
        let Some(pid) = entry.app_pid else {
            return false;
        };
        self.scope = PaletteScope::Application {
            pid,
            previous_query: std::mem::take(&mut self.query),
        };
        self.normalized_query.clear();
        self.rebuild(mru, None);
        true
    }

    pub fn leave_application(&mut self, mru: &PaletteMru) -> bool {
        let previous_query = match std::mem::replace(&mut self.scope, PaletteScope::Root) {
            PaletteScope::Application { previous_query, .. } => previous_query,
            scope => {
                self.scope = scope;
                return false;
            }
        };
        self.query = previous_query;
        self.normalized_query = normalize(&self.query);
        self.rebuild(mru, None);
        true
    }

    pub fn focus_origin(&self) -> Option<PaletteFocusOrigin> { self.snapshot.focus_origin }

    pub fn target_display_id(&self) -> Option<u32> { self.snapshot.target_display_id }

    fn rebuild(&mut self, mru: &PaletteMru, preferred: Option<&PaletteEntryId>) {
        let tokens = self.normalized_query.split_whitespace().collect::<Vec<_>>();
        let mut results = self
            .snapshot
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                match self.scope {
                    PaletteScope::Root => {}
                    PaletteScope::Commands if entry.kind != PaletteEntryKind::Command => {
                        return None;
                    }
                    PaletteScope::Commands => {}
                    PaletteScope::Application { pid, .. }
                        if entry.kind != PaletteEntryKind::Window || entry.app_pid != Some(pid) =>
                    {
                        return None;
                    }
                    PaletteScope::Application { .. } => {}
                }
                let score = if tokens.is_empty() {
                    (entry.show_when_empty || matches!(self.scope, PaletteScope::Commands))
                        .then_some(mru.score(entry))?
                } else {
                    tokens.iter().try_fold(0_i64, |total, token| {
                        best_term_score(token, &entry.search_terms).map(|score| total + score)
                    })? - entry.search_penalty
                };
                let location_score = if entry.is_current_workspace {
                    CURRENT_WORKSPACE_BOOST
                } else if entry.is_current_display {
                    CURRENT_DISPLAY_BOOST
                } else {
                    0
                };
                Some(RankedEntry {
                    index,
                    score: score.saturating_add(location_score),
                })
            })
            .collect::<Vec<_>>();
        results.sort_by(|left, right| {
            right.score.cmp(&left.score).then_with(|| {
                let left_entry = &self.snapshot.entries[left.index];
                let right_entry = &self.snapshot.entries[right.index];
                if left_entry.kind == PaletteEntryKind::Command
                    && right_entry.kind == PaletteEntryKind::Command
                {
                    left.index.cmp(&right.index)
                } else {
                    left_entry.primary.cmp(&right_entry.primary)
                }
            })
        });
        self.results = results;
        self.selection = preferred
            .and_then(|preferred| {
                self.results
                    .iter()
                    .position(|ranked| self.snapshot.entries[ranked.index].id == *preferred)
            })
            .or((!self.results.is_empty()).then_some(0));
    }
}

fn normalize(value: &str) -> String { value.to_lowercase() }

fn best_term_score(query: &str, terms: &[String]) -> Option<i64> {
    terms.iter().filter_map(|term| fuzzy_score(query, term)).max()
}

fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    if query == candidate {
        return Some(10_000);
    }
    if candidate.starts_with(query) {
        return Some(9_000 - candidate.len() as i64);
    }
    if acronym(candidate).starts_with(query) {
        return Some(8_000 - candidate.len() as i64);
    }
    if let Some(index) = candidate.find(query) {
        return Some(7_000 - index as i64 - candidate.len() as i64);
    }

    let mut positions = candidate.char_indices();
    let mut previous = None;
    let mut gaps = 0_i64;
    for needle in query.chars() {
        let (position, _) = positions.find(|(_, current)| *current == needle)?;
        if let Some(previous) = previous {
            gaps += position.saturating_sub(previous + 1) as i64;
        }
        previous = Some(position);
    }
    Some(5_000 - gaps - candidate.len() as i64)
}

fn acronym(candidate: &str) -> String {
    candidate
        .split(|character: char| !character.is_alphanumeric())
        .filter_map(|word| word.chars().next())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::{Duration, Instant};

    use super::*;

    fn window_id(pid: i32, index: u32) -> WindowId {
        WindowId {
            pid,
            idx: NonZeroU32::new(index).unwrap(),
        }
    }

    fn window_entry(pid: i32, index: u32, title: &str) -> PaletteEntry {
        let window = window_id(pid, index);
        PaletteEntry::new(
            PaletteEntryId::Window(window),
            PaletteEntryKind::Window,
            title.to_string(),
            "Ghostty · Terminal · HP Z27k".to_string(),
            ["com.mitchellh.ghostty".to_string()],
            Some(pid),
            PaletteAction::FocusWindow {
                window_id: window,
                window_server_id: None,
            },
        )
    }

    fn app_entry(pid: i32, name: &str, has_windows: bool) -> PaletteEntry {
        let entry = PaletteEntry::new(
            PaletteEntryId::Application(pid),
            PaletteEntryKind::Application,
            name.to_string(),
            "Running application".to_string(),
            [name.to_string()],
            Some(pid),
            PaletteAction::ActivateApplication(pid),
        );
        if has_windows {
            entry.hidden_when_empty()
        } else {
            entry
        }
    }

    fn command_entry(id: &str, label: &str) -> PaletteEntry {
        PaletteEntry::new(
            PaletteEntryId::Command(id.to_string()),
            PaletteEntryKind::Command,
            label.to_string(),
            "Rift Command".to_string(),
            ["command".to_string()],
            None,
            PaletteAction::ReloadConfig,
        )
        .hidden_when_empty()
    }

    #[test]
    fn command_mode_should_show_only_commands_for_an_empty_query() {
        let command = command_entry("reload", "Reload Config");
        let mut model = PaletteModel::standard();
        model.begin_session(
            PaletteSnapshot {
                entries: vec![
                    window_entry(1, 1, "rift"),
                    app_entry(2, "Notes", false),
                    command,
                ],
                ..PaletteSnapshot::default()
            },
            &PaletteMru::default(),
            PaletteMode::Commands,
        );

        assert_eq!(
            model.results().map(|entry| entry.id.clone()).collect::<Vec<_>>(),
            [PaletteEntryId::Command("reload".to_string())]
        );
    }

    #[test]
    fn command_mode_should_ignore_leave_application_navigation() {
        let mut model = PaletteModel::standard();
        model.begin_session(
            PaletteSnapshot {
                entries: vec![command_entry("reload", "Reload Config")],
                ..PaletteSnapshot::default()
            },
            &PaletteMru::default(),
            PaletteMode::Commands,
        );

        let ignored = !model.leave_application(&PaletteMru::default());
        assert!(ignored && matches!(model.scope, PaletteScope::Commands));
    }

    #[test]
    fn command_mode_should_preserve_snapshot_order_for_equal_scores() {
        let mut model = PaletteModel::standard();
        model.begin_session(
            PaletteSnapshot {
                entries: vec![
                    command_entry("workspace.0", "Switch Workspace → Zulu"),
                    command_entry("workspace.1", "Switch Workspace → Alpha"),
                ],
                ..PaletteSnapshot::default()
            },
            &PaletteMru::default(),
            PaletteMode::Commands,
        );

        assert_eq!(
            model.results().map(|entry| entry.id.clone()).collect::<Vec<_>>(),
            [
                PaletteEntryId::Command("workspace.0".to_string()),
                PaletteEntryId::Command("workspace.1".to_string()),
            ]
        );
    }

    #[test]
    fn empty_query_should_hide_apps_that_already_have_windows() {
        let snapshot = PaletteSnapshot {
            entries: vec![window_entry(1, 1, "rift"), app_entry(1, "Ghostty", true)],
            ..PaletteSnapshot::default()
        };
        let mut model = PaletteModel::standard();
        model.set_snapshot(snapshot, &PaletteMru::default());

        assert_eq!(model.results().map(|entry| &entry.id).collect::<Vec<_>>(), [
            &PaletteEntryId::Window(window_id(1, 1)),
        ]);
    }

    #[test]
    fn empty_query_should_rank_mru_window_first() {
        let first = window_id(1, 1);
        let second = window_id(2, 1);
        let snapshot = PaletteSnapshot {
            entries: vec![window_entry(1, 1, "alpha"), window_entry(2, 1, "beta")],
            ..PaletteSnapshot::default()
        };
        let mut mru = PaletteMru::default();
        mru.record_window(first);
        mru.record_window(second);
        let mut model = PaletteModel::standard();
        model.set_snapshot(snapshot, &mru);

        assert_eq!(
            model.selected_entry().map(|entry| &entry.id),
            Some(&PaletteEntryId::Window(second,))
        );
    }

    #[test]
    fn fuzzy_search_should_match_acronym() {
        assert!(fuzzy_score("gw", "ghostty window").is_some());
    }

    #[test]
    fn fuzzy_search_should_match_chinese_directly() {
        assert!(fuzzy_score("飞书", "飞书消息").is_some());
    }

    #[test]
    fn search_should_rank_exact_application_over_fuzzy_window() {
        let snapshot = PaletteSnapshot {
            entries: vec![
                window_entry(1, 1, "ghostty notes"),
                app_entry(1, "ghostty", true),
            ],
            ..PaletteSnapshot::default()
        };
        let mut model = PaletteModel::standard();
        let mru = PaletteMru::default();
        model.set_snapshot(snapshot, &mru);
        model.set_query("ghostty".to_string(), &mru);

        assert_eq!(
            model.selected_entry().map(|entry| entry.kind),
            Some(PaletteEntryKind::Application,)
        );
    }

    #[test]
    fn application_scope_should_only_show_that_apps_windows() {
        let snapshot = PaletteSnapshot {
            entries: vec![
                app_entry(1, "Ghostty", true),
                window_entry(1, 1, "one"),
                window_entry(2, 1, "two"),
            ],
            ..PaletteSnapshot::default()
        };
        let mut model = PaletteModel::standard();
        let mru = PaletteMru::default();
        model.set_snapshot(snapshot, &mru);
        model.set_query("ghostty".to_string(), &mru);
        assert!(model.expand_selected_application(&mru));

        assert_eq!(model.results().map(|entry| &entry.id).collect::<Vec<_>>(), [
            &PaletteEntryId::Window(window_id(1, 1)),
        ]);
    }

    #[test]
    fn leaving_application_scope_should_restore_query() {
        let snapshot = PaletteSnapshot {
            entries: vec![app_entry(1, "Ghostty", true), window_entry(1, 1, "one")],
            ..PaletteSnapshot::default()
        };
        let mut model = PaletteModel::standard();
        let mru = PaletteMru::default();
        model.set_snapshot(snapshot, &mru);
        model.set_query("ghostty".to_string(), &mru);
        assert!(model.expand_selected_application(&mru));
        assert!(model.leave_application(&mru));

        assert_eq!(model.query(), "ghostty");
    }

    #[test]
    fn snapshot_update_should_preserve_selected_entry_identity() {
        let mru = PaletteMru::default();
        let mut model = PaletteModel::standard();
        model.set_snapshot(
            PaletteSnapshot {
                entries: vec![window_entry(1, 1, "alpha"), window_entry(2, 1, "beta")],
                ..PaletteSnapshot::default()
            },
            &mru,
        );
        model.move_selection(1);
        model.set_snapshot(
            PaletteSnapshot {
                entries: vec![
                    window_entry(2, 1, "beta changed"),
                    window_entry(1, 1, "alpha"),
                ],
                ..PaletteSnapshot::default()
            },
            &mru,
        );

        assert_eq!(
            model.selected_entry().map(|entry| &entry.id),
            Some(&PaletteEntryId::Window(window_id(2, 1)),)
        );
    }

    #[test]
    fn application_action_should_prefer_its_mru_window() {
        let first = window_id(1, 1);
        let second = window_id(1, 2);
        let mut mru = PaletteMru::default();
        mru.record_window(first);
        mru.record_window(second);
        let mut model = PaletteModel::standard();
        model.set_snapshot(
            PaletteSnapshot {
                entries: vec![window_entry(1, 1, "first"), window_entry(1, 2, "second")],
                ..PaletteSnapshot::default()
            },
            &mru,
        );

        assert_eq!(
            model.preferred_window_for_application(1, &mru),
            Some(PaletteAction::FocusWindow {
                window_id: second,
                window_server_id: None,
            })
        );
    }

    #[test]
    fn unmatched_query_should_clear_selection() {
        let mru = PaletteMru::default();
        let mut model = PaletteModel::standard();
        model.set_snapshot(
            PaletteSnapshot {
                entries: vec![window_entry(1, 1, "alpha")],
                ..PaletteSnapshot::default()
            },
            &mru,
        );
        model.set_query("missing".to_string(), &mru);

        assert!(model.selected_entry().is_none());
    }

    #[test]
    fn filtering_should_stay_interactive_with_five_hundred_entries() {
        let entries = (1..=500)
            .map(|index| window_entry(index, 1, &format!("project window {index}")))
            .collect();
        let mru = PaletteMru::default();
        let mut model = PaletteModel::standard();
        model.set_snapshot(
            PaletteSnapshot {
                entries,
                ..PaletteSnapshot::default()
            },
            &mru,
        );
        let started = Instant::now();
        for query in ["p", "pr", "pro", "proj", "project", "project 42"].repeat(20) {
            model.set_query(query.to_string(), &mru);
        }
        let elapsed = started.elapsed();
        eprintln!("120 palette filters over 500 entries: {elapsed:?}");

        assert!(elapsed < Duration::from_millis(800));
    }

    #[test]
    fn ranking_should_preserve_the_complete_result_set_for_scrolling() {
        let entries = (1..=25)
            .map(|index| window_entry(index, 1, &format!("window {index}")))
            .collect();
        let mut model = PaletteModel::standard();
        model.set_snapshot(
            PaletteSnapshot {
                entries,
                ..PaletteSnapshot::default()
            },
            &PaletteMru::default(),
        );

        assert_eq!(model.results().count(), 25);
        model.move_selection(24);
        assert_eq!(model.selected_index(), Some(24));
    }
}
