//! Pure state transitions. Requests are effects drained by the event loop.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use super::worker::{PlanPurpose, Request, Row, RunEvent, Snapshot, WorkerEvent, plan_request};
use crate::pipeline::{ReviewedPlan, SelectionSummary};
use cleanrs_core::execution::ActionResult;
use cleanrs_core::fs::{NodeIndex, ScanProgress};
use cleanrs_core::journal::JournalRun;
use cleanrs_core::targets::{
    CandidateKey, CleanupLevel, PlanSelection, PlannedTarget, SelectionSource,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Browser,
    Targets,
    Review,
    Run,
    Results,
    Help,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Move(i32),
    First,
    Last,
    HalfPage(i32),
    Open,
    Parent,
    Previous,
    StartFilter,
    FilterChar(char),
    FilterBackspace,
    ConfirmFilter,
    ClearFilter,
    Match(i32),
    Sort,
    Info,
    Rescan,
    Targets,
    Toggle,
    Visual,
    SelectAll,
    Invert,
    ClearSelection,
    Preset(CleanupLevel),
    Machine,
    Review,
    Confirm,
    Cancel,
    OutputScroll(i32),
    Help,
    Back,
    Quit,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Size,
    Name,
    Age,
}
impl Sort {
    fn next(self) -> Self {
        match self {
            Self::Size => Self::Name,
            Self::Name => Self::Age,
            Self::Age => Self::Size,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Size => "allocated",
            Self::Name => "name",
            Self::Age => "age (oldest first)",
        }
    }
}

pub struct Directory {
    pub revision: u64,
    pub rows: Vec<Row>,
    pub listed: bool,
    pub total: u64,
    pub volume: String,
    pub finished: Instant,
    pub progress: HashMap<PathBuf, ScanProgress>,
    pub selected: Option<PathBuf>,
    pub cursor_moved: bool,
    pub coverage_error: bool,
}
impl Directory {
    fn empty(revision: u64) -> Self {
        Self {
            revision,
            rows: Vec::new(),
            listed: false,
            total: 0,
            volume: "Free unknown · unknown".into(),
            finished: Instant::now(),
            progress: HashMap::new(),
            selected: None,
            cursor_moved: false,
            coverage_error: false,
        }
    }
    pub fn busy(&self) -> bool {
        !self.listed || self.rows.iter().any(|row| row.pending)
    }
}

/// Session-local targets cache. The baseline is the level-only dry run; the
/// summary uses cached set accounting; review rebuilds through the pipeline
/// using every tick as an explicit manual choice. Cached facts cannot execute.
pub struct Targets {
    pub level: CleanupLevel,
    pub machine: bool,
    pub baseline: Option<Arc<ReviewedPlan>>,
    pub selected: HashSet<CandidateKey>,
    pub summary: Option<SelectionSummary>,
    pub previous_ticks: Option<(HashSet<CandidateKey>, HashSet<CandidateKey>)>,
    pub highlighted: Option<CandidateKey>,
    pub visual: Option<CandidateKey>,
    pub filter: String,
    pub sort: Sort,
    pub generation: u64,
    pub planning: bool,
    pub reviewing: bool,
    pub focus_path: Option<PathBuf>,
    pub activity: String,
    pub finished: Instant,
}
impl Default for Targets {
    fn default() -> Self {
        Self {
            level: CleanupLevel::Safe,
            machine: false,
            baseline: None,
            selected: HashSet::new(),
            summary: None,
            previous_ticks: None,
            highlighted: None,
            visual: None,
            filter: String::new(),
            sort: Sort::Size,
            generation: 0,
            planning: false,
            reviewing: false,
            focus_path: None,
            activity: "detecting…".into(),
            finished: Instant::now(),
        }
    }
}
#[derive(Debug, Clone)]
pub enum RunAction {
    Pending,
    Running,
    Finished(ActionResult),
}
pub struct Run {
    pub plan: Arc<ReviewedPlan>,
    pub stop: Arc<AtomicBool>,
    pub confirm_stop: bool,
    pub actions: Vec<RunAction>,
    pub output: VecDeque<String>,
    pub output_scroll: usize,
    pub scroll: usize,
    pub summary: Option<JournalRun>,
    pub error: Option<String>,
}

/// Group from the returned plan only, including explicit-only properties.
pub fn review_group(source: &SelectionSource, explicit_only: bool) -> usize {
    if explicit_only {
        2
    } else if matches!(source, SelectionSource::Manual { overrode: Some(_) }) {
        1
    } else {
        0
    }
}

// Bound both arena storage and its aliases. The visible directory/review and
// worker-owned scans are working data, not cache, and can exceed these limits.
const CACHE_NODES: usize = 250_000;
const CACHE_PATHS: usize = 4_096;
const CACHE_LISTINGS: usize = 32;
const CACHE_TARGET_SCOPES: usize = 8;

pub struct App {
    pub current: PathBuf,
    pub home: Option<PathBuf>,
    pub target_scope: Option<PathBuf>,
    pub targets_cache: HashMap<PathBuf, Targets>,
    pub reviewed: Option<Arc<ReviewedPlan>>,
    pub review_scroll: usize,
    pub review_notice: Option<String>,
    pub help_scroll: usize,
    pub run: Option<Run>,
    generation: u64,
    pub directories: HashMap<PathBuf, Directory>,
    cache: HashMap<PathBuf, (Arc<Snapshot>, NodeIndex)>,
    pub screens: Vec<Screen>,
    pub history: Vec<PathBuf>,
    pub sort: Sort,
    pub filter: String,
    pub filtering: bool,
    pub info: bool,
    pub status: Option<(String, bool)>,
    pub quit: bool,
    pub tick: usize,
    pub page_rows: usize,
    pub page_width: u16,
    revision: u64,
    effects: Vec<Request>,
    explaining: Option<PathBuf>,
}
impl App {
    pub fn new(root: PathBuf) -> Self {
        let mut app = Self {
            current: root,
            home: None,
            target_scope: None,
            targets_cache: HashMap::new(),
            reviewed: None,
            review_scroll: 0,
            review_notice: None,
            help_scroll: 0,
            run: None,
            generation: 0,
            directories: HashMap::new(),
            cache: HashMap::new(),
            screens: vec![Screen::Browser],
            history: Vec::new(),
            sort: Sort::Size,
            filter: String::new(),
            filtering: false,
            info: false,
            status: None,
            quit: false,
            tick: 0,
            page_width: 80,
            page_rows: 20,
            revision: 0,
            effects: Vec::new(),
            explaining: None,
        };
        app.ensure_current();
        app
    }
    pub fn screen(&self) -> Screen {
        *self.screens.last().expect("browser screen retained")
    }
    pub fn directory(&self) -> &Directory {
        &self.directories[&self.current]
    }
    pub fn busy(&self) -> bool {
        self.directories.values().any(Directory::busy)
            || self.targets_cache.values().any(|t| t.planning)
            || self.screens.contains(&Screen::Run)
    }
    pub fn drain_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.effects)
    }

    pub fn visible(&self) -> Vec<&Row> {
        let filter = self.filter.to_lowercase();
        let mut rows: Vec<_> = self
            .directory()
            .rows
            .iter()
            .filter(|row| row.name.to_lowercase().contains(&filter))
            .collect();
        rows.sort_by(|a, b| {
            match self.sort {
                Sort::Size => b.summary.allocated.cmp(&a.summary.allocated),
                Sort::Name => a.name.cmp(&b.name),
                Sort::Age => a.modified.cmp(&b.modified),
            }
            .then_with(|| a.path.cmp(&b.path))
        });
        rows
    }
    pub fn highlighted(&self) -> Option<&Row> {
        let selected = self.directory().selected.as_ref();
        self.visible()
            .into_iter()
            .find(|row| Some(&row.path) == selected)
    }
    fn reconcile_cursor(&mut self) {
        let rows = self.visible();
        let selected = self.directory().selected.as_ref();
        let keep = rows.iter().any(|row| Some(&row.path) == selected);
        let first = rows.first().map(|row| row.path.clone());
        if !keep || !self.directory().cursor_moved {
            self.directories.get_mut(&self.current).unwrap().selected = first;
        }
        self.request_explanation();
    }
    fn move_cursor(&mut self, delta: i32, wrap: bool) {
        let rows = self.visible();
        if rows.is_empty() {
            return;
        }
        let current = rows
            .iter()
            .position(|row| Some(&row.path) == self.directory().selected.as_ref())
            .unwrap_or(0) as i64;
        let next = current + i64::from(delta);
        let next = if wrap {
            next.rem_euclid(rows.len() as i64)
        } else {
            next.clamp(0, rows.len() as i64 - 1)
        } as usize;
        let path = rows[next].path.clone();
        self.directories
            .get_mut(&self.current)
            .unwrap()
            .cursor_moved = true;
        self.directories.get_mut(&self.current).unwrap().selected = Some(path);
        self.request_explanation();
    }

    pub fn handle(&mut self, action: Action) {
        self.status = None;
        if self.screen() == Screen::Help {
            let context = self
                .screens
                .iter()
                .rev()
                .find(|s| **s != Screen::Help)
                .copied()
                .unwrap_or(Screen::Browser);
            let max_scroll = super::keymap::bindings(context)
                .count()
                .saturating_add(1)
                .saturating_sub(self.page_rows);
            self.help_scroll = self.help_scroll.min(max_scroll);
            if action == Action::Quit {
                self.screens.pop();
                self.handle(Action::Quit);
            } else if matches!(action, Action::Help | Action::Back) {
                self.screens.pop();
            } else {
                match action {
                    Action::Move(d) => {
                        self.help_scroll = self
                            .help_scroll
                            .saturating_add_signed(d as isize)
                            .min(max_scroll)
                    }
                    Action::HalfPage(d) => {
                        self.help_scroll = self
                            .help_scroll
                            .saturating_add_signed((d * self.page_rows as i32 / 2) as isize)
                            .min(max_scroll)
                    }
                    Action::First => self.help_scroll = 0,
                    Action::Last => self.help_scroll = max_scroll,
                    _ => {}
                }
            }
            return;
        }
        if action == Action::Quit && self.screen() != Screen::Run {
            self.quit = true;
            self.effects.push(Request::Quit);
            return;
        }
        if action == Action::Help {
            self.help_scroll = 0;
            self.screens.push(Screen::Help);
            return;
        }
        if self.screen() != Screen::Browser {
            self.handle_cleanup(action);
            return;
        }
        match action {
            Action::Move(delta) => self.move_cursor(delta, false),
            Action::HalfPage(direction) => {
                self.move_cursor(direction * (self.page_rows.max(2) / 2) as i32, false)
            }
            Action::Match(delta) => self.move_cursor(delta, true),
            Action::First | Action::Last => {
                let rows = self.visible();
                let row = if action == Action::First {
                    rows.first()
                } else {
                    rows.last()
                };
                let selected = row.map(|row| row.path.clone());
                self.directories
                    .get_mut(&self.current)
                    .unwrap()
                    .cursor_moved = true;
                self.directories.get_mut(&self.current).unwrap().selected = selected;
                self.request_explanation();
            }
            Action::Open => {
                if let Some(row) = self.highlighted() {
                    if row.navigable() {
                        self.navigate(row.path.clone(), true);
                    } else {
                        self.status = Some(("entry is a file, symlink, cloud placeholder, firmlink, or mount boundary".into(), false));
                    }
                }
            }
            Action::Parent => {
                if let Some(parent) = self.current.parent() {
                    self.navigate(parent.to_path_buf(), true);
                }
            }
            Action::Previous => {
                if let Some(previous) = self.history.pop() {
                    self.navigate(previous, false);
                }
            }
            Action::StartFilter => self.filtering = true,
            Action::FilterChar(c) => {
                self.filter.push(c);
                self.reconcile_cursor();
            }
            Action::FilterBackspace => {
                self.filter.pop();
                self.reconcile_cursor();
            }
            Action::ConfirmFilter => self.filtering = false,
            Action::ClearFilter => {
                self.filter.clear();
                self.filtering = false;
                self.reconcile_cursor();
            }
            Action::Sort => {
                self.sort = self.sort.next();
                self.reconcile_cursor();
            }
            Action::Info => {
                self.info = !self.info;
                self.request_explanation();
            }
            Action::Rescan => {
                self.revision += 1;
                let current = &self.current;
                // Invalidate overlapping ancestor arenas as well as descendants.
                // Their old subtree must never replace an explicitly refreshed one.
                self.cache
                    .retain(|path, _| !path.starts_with(current) && !current.starts_with(path));
                self.directories
                    .retain(|path, _| !path.starts_with(current) && !current.starts_with(path));
                self.directories
                    .insert(self.current.clone(), Directory::empty(self.revision));
                self.explaining = None;
                self.effects.push(Request::Rescan {
                    path: self.current.clone(),
                    revision: self.revision,
                });
            }
            Action::Targets => self.open_targets(),
            Action::Back => {
                if self.screens.len() > 1 {
                    self.screens.pop();
                } else {
                    self.quit = true;
                    self.effects.push(Request::Quit);
                }
            }
            Action::Quit => {
                self.quit = true;
                self.effects.push(Request::Quit);
            }
            _ => {}
        }
    }

    fn trim_caches(&mut self) {
        // Charge a shared snapshot once, then evict every alias together. Merely
        // removing one path would leave the same arena retained by descendants.
        loop {
            let snapshots: HashMap<_, _> = self
                .cache
                .values()
                .map(|(s, _)| (Arc::as_ptr(s), s.clone()))
                .collect();
            let nodes: usize = snapshots.values().map(|s| s.scan.arena.nodes.len()).sum();
            if nodes <= CACHE_NODES && self.cache.len() <= CACHE_PATHS {
                break;
            }
            let Some(oldest) = snapshots.values().min_by_key(|s| s.finished).cloned() else {
                break;
            };
            self.cache.retain(|_, (s, _)| !Arc::ptr_eq(s, &oldest));
            self.directories.retain(|path, d| {
                path == &self.current
                    || !d.rows.iter().any(|r| {
                        r.source
                            .as_ref()
                            .is_some_and(|(s, _)| Arc::ptr_eq(s, &oldest))
                    })
            });
            self.revision += 1;
        }
        let retained: HashSet<_> = self.cache.values().map(|(s, _)| Arc::as_ptr(s)).collect();
        self.directories.retain(|path, d| {
            path == &self.current
                || !d.rows.iter().any(|r| {
                    r.source
                        .as_ref()
                        .is_some_and(|(s, _)| !retained.contains(&Arc::as_ptr(s)))
                })
        });
        // Directory listings also retain row sources; bound them independently.
        while self.directories.len() > CACHE_LISTINGS
            || self
                .directories
                .iter()
                .filter(|(p, _)| *p != &self.current)
                .map(|(_, d)| d.rows.len())
                .sum::<usize>()
                > CACHE_NODES
        {
            let Some(path) = self
                .directories
                .iter()
                .filter(|(p, _)| *p != &self.current)
                .min_by_key(|(_, d)| d.finished)
                .map(|(p, _)| p.clone())
            else {
                break;
            };
            self.directories.remove(&path);
            self.revision += 1;
        }
        loop {
            let nodes: usize = self
                .targets_cache
                .values()
                .filter_map(|t| t.baseline.as_ref())
                .map(|p| p.accounting.node_count())
                .sum();
            if self.targets_cache.len() <= CACHE_TARGET_SCOPES && nodes <= CACHE_NODES {
                break;
            }
            let active_targets = self.screens.contains(&Screen::Targets);
            let Some(path) = self
                .targets_cache
                .iter()
                .filter(|(p, _)| !active_targets || Some(*p) != self.target_scope.as_ref())
                .min_by_key(|(_, t)| t.finished)
                .map(|(p, _)| p.clone())
            else {
                break;
            };
            self.targets_cache.remove(&path);
        }
    }

    fn navigate(&mut self, path: PathBuf, remember: bool) {
        if path == self.current {
            return;
        }
        if remember {
            self.history.push(self.current.clone());
        }
        self.current = path;
        self.filter.clear();
        self.filtering = false;
        self.explaining = None;
        self.ensure_current();
        self.trim_caches();
        self.reconcile_cursor();
    }
    fn ensure_current(&mut self) {
        if let Some((snapshot, index)) = self.cache.get(&self.current) {
            let selected = self
                .directories
                .get(&self.current)
                .and_then(|d| d.selected.clone());
            self.directories.insert(
                self.current.clone(),
                Directory {
                    revision: self.revision,
                    rows: snapshot.rows(*index),
                    listed: true,
                    total: snapshot.summaries[index.as_usize()].allocated,
                    volume: snapshot.volume.clone(),
                    finished: snapshot.finished,
                    progress: HashMap::new(),
                    cursor_moved: self
                        .directories
                        .get(&self.current)
                        .is_some_and(|d| d.cursor_moved),
                    selected,
                    coverage_error: snapshot.summaries[index.as_usize()].incomplete,
                },
            );
        } else if !self.directories.contains_key(&self.current) {
            self.directories
                .insert(self.current.clone(), Directory::empty(self.revision));
            self.effects.push(Request::Browse {
                path: self.current.clone(),
                revision: self.revision,
            });
        } else if self.directory().busy() {
            self.effects.push(Request::Browse {
                path: self.current.clone(),
                revision: self.directory().revision,
            });
        }
    }
    fn request_explanation(&mut self) {
        if !self.info {
            return;
        }
        let Some(row) = self.highlighted() else {
            return;
        };
        if row.estimate.is_some() || self.explaining.as_ref() == Some(&row.path) {
            return;
        }
        if let Some((snapshot, index)) = &row.source {
            let snapshot = Arc::clone(snapshot);
            let index = *index;
            let path = row.path.clone();
            self.explaining = Some(path);
            self.effects.push(Request::Explain {
                path: self.current.clone(),
                revision: self.directory().revision,
                snapshot,
                index,
            });
        }
    }

    pub fn on_worker(&mut self, event: WorkerEvent) {
        if matches!(
            event,
            WorkerEvent::Planning { .. } | WorkerEvent::Planned { .. } | WorkerEvent::Run { .. }
        ) {
            self.on_cleanup_worker(event);
            return;
        }
        let (path, revision) = match &event {
            WorkerEvent::Listed { path, revision, .. } => (path, revision),
            WorkerEvent::Progress {
                parent, revision, ..
            }
            | WorkerEvent::ChildFinished {
                parent, revision, ..
            }
            | WorkerEvent::Explained {
                parent, revision, ..
            }
            | WorkerEvent::Coverage { parent, revision }
            | WorkerEvent::Error {
                parent, revision, ..
            } => (parent, revision),
            _ => unreachable!("cleanup events handled separately"),
        };
        if !self
            .directories
            .get(path)
            .is_some_and(|directory| directory.revision == *revision)
        {
            return;
        }
        match event {
            WorkerEvent::Listed {
                path,
                rows,
                total,
                volume,
                ..
            } => {
                let directory = self.directories.get_mut(&path).unwrap();
                directory.rows = rows;
                directory.total = total;
                directory.volume = volume;
                directory.listed = true;
                directory.finished = Instant::now();
            }
            WorkerEvent::Progress {
                parent,
                child,
                progress,
                ..
            } => {
                self.directories
                    .get_mut(&parent)
                    .unwrap()
                    .progress
                    .insert(child, progress);
            }
            WorkerEvent::ChildFinished {
                parent,
                path,
                snapshot,
                total,
                ..
            } => {
                let directory = self.directories.get_mut(&parent).unwrap();
                if let Some(row) = directory.rows.iter_mut().find(|row| row.path == path) {
                    row.pending = false;
                    let target_id = row.summary.target_id.clone();
                    row.summary = snapshot.summaries[0].clone();
                    row.summary.target_id = target_id.or(row.summary.target_id.take());
                    row.source = Some((Arc::clone(&snapshot), snapshot.scan.arena.root()));
                    row.estimate = None;
                }
                directory.total = total;
                directory.finished = snapshot.finished;
                // Completed counters remain in the activity sum until all jobs
                // finish. For tiny jobs no observer tick arrives, so fill them.
                directory.progress.insert(
                    path,
                    ScanProgress {
                        directories: snapshot.directories.len() as u64,
                        entries: snapshot.scan.arena.nodes.len().saturating_sub(1) as u64,
                        allocated_bytes: snapshot.summaries[0].allocated,
                    },
                );
                for (path, index) in &snapshot.directories {
                    self.cache
                        .insert(path.clone(), (Arc::clone(&snapshot), *index));
                }
                // Descending early may have created its own progressive listing.
                // Leave those jobs/results intact; future visits use this cache.
            }
            WorkerEvent::Explained {
                parent,
                path,
                estimate,
                ..
            } => {
                if let Some(row) = self
                    .directories
                    .get_mut(&parent)
                    .unwrap()
                    .rows
                    .iter_mut()
                    .find(|row| row.path == path)
                {
                    row.estimate = Some(estimate);
                }
                if self.explaining.as_ref() == Some(&path) {
                    self.explaining = None;
                }
            }
            WorkerEvent::Coverage { parent, .. } => {
                let directory = self.directories.get_mut(&parent).unwrap();
                directory.coverage_error = true;
                directory.listed = true;
            }
            WorkerEvent::Error {
                parent,
                child,
                message,
                ..
            } => {
                let directory = self.directories.get_mut(&parent).unwrap();
                directory.coverage_error = true;
                directory.listed = true;
                if let Some(child) = child {
                    if let Some(row) = directory.rows.iter_mut().find(|row| row.path == child) {
                        row.pending = false;
                        row.summary.incomplete = true;
                    }
                }
                self.status = Some((message, true));
            }
            _ => unreachable!("cleanup events handled separately"),
        }
        self.trim_caches();
        self.reconcile_cursor();
    }
}

impl App {
    pub fn targets(&self) -> Option<&Targets> {
        self.target_scope
            .as_ref()
            .and_then(|p| self.targets_cache.get(p))
    }
    fn targets_mut(&mut self) -> &mut Targets {
        self.targets_cache
            .get_mut(self.target_scope.as_ref().expect("targets scope"))
            .expect("targets cache")
    }
    pub fn target_rows(&self) -> Vec<&PlannedTarget> {
        let Some(targets) = self.targets() else {
            return Vec::new();
        };
        let Some(plan) = &targets.baseline else {
            return Vec::new();
        };
        let filter = targets.filter.to_lowercase();
        let mut rows: Vec<_> = plan
            .targets
            .iter()
            .filter(|row| {
                format!(
                    "{} {} {}",
                    row.target.id,
                    row.target
                        .project_root
                        .as_ref()
                        .map_or_else(String::new, |p| p.display().to_string()),
                    row.target
                        .path
                        .as_ref()
                        .map_or_else(String::new, |p| p.display().to_string())
                )
                .to_lowercase()
                .contains(&filter)
            })
            .collect();
        rows.sort_by(|a, b| {
            match targets.sort {
                Sort::Size => b
                    .target
                    .usage
                    .sortable_bytes()
                    .cmp(&a.target.usage.sortable_bytes()),
                Sort::Name => a
                    .target
                    .project_root
                    .cmp(&b.target.project_root)
                    .then_with(|| a.target.id.cmp(&b.target.id)),
                Sort::Age => b.target.age_days.cmp(&a.target.age_days),
            }
            .then_with(|| a.candidate_key.cmp(&b.candidate_key))
        });
        rows
    }
    fn reconcile_target_cursor(&mut self) {
        let rows = self.target_rows();
        let selected = self.targets().and_then(|t| t.highlighted.as_ref());
        if !rows.iter().any(|r| Some(&r.candidate_key) == selected) {
            let first = rows.first().map(|r| r.candidate_key.clone());
            self.targets_mut().highlighted = first;
        }
    }
    fn move_target_cursor(&mut self, delta: i32, wrap: bool) {
        let rows = self.target_rows();
        if rows.is_empty() {
            return;
        }
        let current = rows
            .iter()
            .position(|r| Some(&r.candidate_key) == self.targets().unwrap().highlighted.as_ref())
            .unwrap_or(0) as i64;
        let next = current + i64::from(delta);
        let next = if wrap {
            next.rem_euclid(rows.len() as i64)
        } else {
            next.clamp(0, rows.len() as i64 - 1)
        };
        let key = rows[next as usize].candidate_key.clone();
        self.targets_mut().highlighted = Some(key);
    }
    fn open_targets(&mut self) {
        let row = self.highlighted();
        let focus_path = row
            .filter(|r| r.summary.target_id.is_some())
            .map(|r| r.path.clone());
        let scope = row
            .filter(|r| {
                r.kind == cleanrs_core::fs::NodeKind::Directory && r.summary.target_id.is_none()
            })
            .map_or_else(|| self.current.clone(), |r| r.path.clone());
        self.target_scope = Some(scope.clone());
        self.filtering = false;
        self.screens.push(Screen::Targets);
        if let std::collections::hash_map::Entry::Vacant(entry) = self.targets_cache.entry(scope) {
            entry.insert(Targets::default());
            self.request_plan(PlanPurpose::Detect);
        }
        self.targets_mut().focus_path = focus_path;
        self.focus_target_path();
        self.reconcile_target_cursor();
    }
    fn focus_target_path(&mut self) {
        let targets = self.targets_mut();
        if let (Some(path), Some(plan)) = (&targets.focus_path, &targets.baseline) {
            if let Some(row) = plan
                .targets
                .iter()
                .find(|r| r.target.path.as_ref() == Some(path))
            {
                targets.highlighted = Some(row.candidate_key.clone());
            }
            targets.focus_path = None;
        }
    }
    fn request_plan(&mut self, purpose: PlanPurpose) {
        self.generation += 1;
        let generation = self.generation;
        let scope = self.target_scope.clone().unwrap();
        let home = self.home.clone();
        let browser_scan = (purpose == PlanPurpose::Detect
            && self.targets().is_some_and(|t| t.generation == 0))
        .then(|| {
            let sources = if let Some((snapshot, index)) =
                self.cache.get(&scope).cloned().or_else(|| {
                    self.directory()
                        .rows
                        .iter()
                        .find(|r| r.path == scope)
                        .and_then(|r| r.source.clone())
                }) {
                snapshot
                    .scan
                    .arena
                    .children(index)
                    .filter(|i| {
                        snapshot.scan.arena.nodes[i.as_usize()].kind
                            == cleanrs_core::fs::NodeKind::Directory
                    })
                    .map(|i| (snapshot.clone(), i))
                    .collect()
            } else {
                self.directories
                    .get(&scope)
                    .filter(|d| !d.busy())
                    .map(|d| d.rows.iter().filter_map(|r| r.source.clone()).collect())
                    .unwrap_or_default()
            };
            super::worker::BrowserScan { sources }
        });
        let targets = self.targets_mut();
        targets.generation = generation;
        targets.planning = true;
        targets.reviewing = purpose == PlanPurpose::Review;
        targets.activity = if purpose == PlanPurpose::Detect {
            "detecting…"
        } else {
            "preparing…"
        }
        .into();
        let request = plan_request(
            scope,
            home,
            targets.level,
            targets.machine,
            (purpose != PlanPurpose::Detect).then(|| targets.selected.clone()),
        );
        let detection = targets.baseline.clone();
        self.effects.push(Request::Plan {
            browser_scan,
            detection,
            generation,
            purpose,
            request: Box::new(request),
        });
    }
    fn selection_changed(&mut self) {
        if self.targets().is_some_and(|t| t.baseline.is_none()) {
            return;
        }
        // Reject any in-flight review for an older tick set immediately. Live
        // totals read only cached detection facts and never enqueue scan work.
        self.generation += 1;
        let generation = self.generation;
        let targets = self.targets_mut();
        targets.generation = generation;
        targets.planning = false;
        targets.summary = targets.baseline.as_ref().map(|plan| {
            plan.accounting.summarize(
                plan.targets
                    .iter()
                    .filter(|r| targets.selected.contains(&r.candidate_key)),
            )
        });
    }
    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }
    pub fn visual_keys(&self) -> HashSet<CandidateKey> {
        let Some(targets) = self.targets() else {
            return HashSet::new();
        };
        let rows = self.target_rows();
        let anchor = rows
            .iter()
            .position(|r| Some(&r.candidate_key) == targets.visual.as_ref());
        let cursor = rows
            .iter()
            .position(|r| Some(&r.candidate_key) == targets.highlighted.as_ref());
        match (anchor, cursor) {
            (Some(a), Some(b)) => rows[a.min(b)..=a.max(b)]
                .iter()
                .filter(|r| r.target.manually_selectable())
                .map(|r| r.candidate_key.clone())
                .collect(),
            _ => HashSet::new(),
        }
    }
    fn toggle_target(&mut self) {
        if self.targets().unwrap().visual.is_some() {
            let range = self.visual_keys();
            self.targets_mut().selected.extend(range);
            self.targets_mut().visual = None;
            self.selection_changed();
            return;
        }
        let key = self.targets().unwrap().highlighted.clone();
        let allowed = self
            .target_rows()
            .iter()
            .any(|r| Some(&r.candidate_key) == key.as_ref() && r.target.manually_selectable());
        if allowed {
            let key = key.unwrap();
            if !self.targets_mut().selected.remove(&key) {
                self.targets_mut().selected.insert(key);
            }
            self.selection_changed();
        } else {
            self.status = Some((
                "refused or informational item cannot be selected".into(),
                true,
            ));
        }
        self.move_target_cursor(1, false);
    }
    fn handle_cleanup(&mut self, action: Action) {
        match self.screen() {
            Screen::Targets => match action {
                Action::Move(delta) => self.move_target_cursor(delta, false),
                Action::Match(delta) => self.move_target_cursor(delta, true),
                Action::HalfPage(d) => {
                    self.move_target_cursor(d * (self.page_rows.max(2) / 2) as i32, false)
                }
                Action::First | Action::Last => {
                    let rows = self.target_rows();
                    let key = if action == Action::First {
                        rows.first()
                    } else {
                        rows.last()
                    }
                    .map(|r| r.candidate_key.clone());
                    self.targets_mut().highlighted = key;
                }
                Action::Toggle => self.toggle_target(),
                Action::Visual => {
                    if self.targets().unwrap().visual.is_some() {
                        self.toggle_target();
                    } else {
                        self.targets_mut().visual = self.targets().unwrap().highlighted.clone();
                    }
                }
                Action::SelectAll | Action::Invert => {
                    let keys: Vec<_> = self
                        .targets()
                        .unwrap()
                        .baseline
                        .as_ref()
                        .into_iter()
                        .flat_map(|p| p.targets.iter())
                        .filter(|r| {
                            r.target.manually_selectable()
                                && (action == Action::Invert
                                    || matches!(r.selection, PlanSelection::Selected { .. }))
                        })
                        .map(|r| r.candidate_key.clone())
                        .collect();
                    for key in keys {
                        if action == Action::SelectAll || !self.targets_mut().selected.remove(&key)
                        {
                            self.targets_mut().selected.insert(key);
                        }
                    }
                    self.selection_changed();
                }
                Action::ClearSelection => {
                    self.targets_mut().selected.clear();
                    self.selection_changed();
                }
                Action::Preset(level) => {
                    self.targets_mut().level = level;
                    let targets = self.targets_mut();
                    if let Some(plan) = &targets.baseline {
                        let mut baseline = (**plan).clone();
                        baseline.level = level;
                        // Policy presets were resolved by the detection worker
                        // through the core planner. Preserve its row order and
                        // cached keys; this path does no filesystem I/O.
                        for (row, selection) in baseline
                            .targets
                            .iter_mut()
                            .zip(baseline.accounting.preset(level))
                        {
                            row.selection = selection.clone();
                        }
                        targets.selected = baseline
                            .targets
                            .iter()
                            .filter(|r| matches!(r.selection, PlanSelection::Selected { .. }))
                            .map(|r| r.candidate_key.clone())
                            .collect();
                        targets.baseline = Some(Arc::new(baseline));
                        targets.previous_ticks = None;
                        self.selection_changed();
                    } else {
                        self.reset_detection(false);
                    }
                    self.status =
                        Some(("level preset applied; manual ticks discarded".into(), false));
                }
                Action::Machine => {
                    self.targets_mut().machine = !self.targets().unwrap().machine;
                    self.reset_detection(true);
                }
                Action::Rescan => self.reset_detection(true),
                Action::Review => {
                    if self.targets().unwrap().selected.is_empty() {
                        self.status =
                            Some(("Select at least one item before review".into(), false));
                    } else if self.targets().unwrap().baseline.is_none() {
                        self.status = Some(("Detection is still running".into(), false));
                    } else {
                        self.request_plan(PlanPurpose::Review);
                    }
                }
                Action::StartFilter => self.filtering = true,
                Action::FilterChar(c) => {
                    self.targets_mut().filter.push(c);
                    self.reconcile_target_cursor();
                }
                Action::FilterBackspace => {
                    self.targets_mut().filter.pop();
                    self.reconcile_target_cursor();
                }
                Action::ConfirmFilter => self.filtering = false,
                Action::ClearFilter => {
                    self.targets_mut().filter.clear();
                    self.filtering = false;
                    self.reconcile_target_cursor();
                }
                Action::Sort => {
                    self.targets_mut().sort = self.targets().unwrap().sort.next();
                    self.reconcile_target_cursor();
                }
                Action::Back | Action::Quit => {
                    if self.targets().unwrap().visual.is_some() {
                        self.targets_mut().visual = None;
                    } else {
                        self.screens.pop();
                        self.filtering = false;
                        self.trim_caches();
                    }
                }
                _ => {}
            },
            Screen::Review => match action {
                Action::Confirm => self.start_run(),
                Action::Move(d) => {
                    self.review_scroll = self.review_scroll.saturating_add_signed(d as isize)
                }
                Action::HalfPage(d) => {
                    self.review_scroll = self
                        .review_scroll
                        .saturating_add_signed((d * self.page_rows as i32 / 2) as isize)
                }
                Action::First => self.review_scroll = 0,
                Action::Last => {
                    self.review_scroll = super::views::review::scroll_limit(
                        self,
                        self.page_width.saturating_sub(2),
                        self.page_rows,
                    );
                }
                Action::Cancel | Action::Back | Action::Quit => {
                    self.screens.pop();
                    self.reviewed = None;
                }
                _ => {} // Enter is deliberately inert at the destructive boundary.
            },
            Screen::Run => {
                let run = self.run.as_mut().unwrap();
                match action {
                    Action::Back | Action::Quit => run.confirm_stop = true,
                    Action::Confirm if run.confirm_stop => {
                        run.stop.store(true, Ordering::Release);
                        run.confirm_stop = false;
                        self.status = Some(("Stopping after the current action".into(), false));
                    }
                    Action::Cancel => run.confirm_stop = false,
                    Action::Move(d) => {
                        run.scroll = run
                            .scroll
                            .saturating_add_signed(d as isize)
                            .min(run.actions.len().saturating_sub(1))
                    }
                    Action::HalfPage(d) => {
                        run.scroll = run
                            .scroll
                            .saturating_add_signed((d * self.page_rows as i32 / 2) as isize)
                            .min(run.actions.len().saturating_sub(1))
                    }
                    Action::OutputScroll(d) => {
                        run.output_scroll = run
                            .output_scroll
                            .saturating_add_signed(d as isize)
                            .min(run.output.len().saturating_sub(1))
                    }
                    _ => {}
                }
            }
            Screen::Results => match action {
                Action::Back | Action::Quit => {
                    self.screens.truncate(1);
                    self.reviewed = None;
                    self.return_after_run();
                    self.run = None;
                }
                Action::Move(d) => {
                    let run = self.run.as_mut().unwrap();
                    run.scroll = run.scroll.saturating_add_signed(d as isize).min(
                        run.summary
                            .as_ref()
                            .map_or(0, |s| s.actions.len().saturating_sub(1)),
                    );
                }
                _ => {}
            },
            _ => {}
        }
    }
    fn reset_detection(&mut self, preserve_ticks: bool) {
        let targets = self.targets_mut();
        targets.previous_ticks = if preserve_ticks {
            targets.baseline.as_ref().map(|p| {
                (
                    targets.selected.clone(),
                    p.targets.iter().map(|r| r.candidate_key.clone()).collect(),
                )
            })
        } else {
            None
        };
        targets.baseline = None;
        targets.summary = None;
        targets.selected.clear();
        targets.visual = None;
        self.request_plan(PlanPurpose::Detect);
    }
    fn start_run(&mut self) {
        let Some(plan) = self.reviewed.clone() else {
            return;
        };
        // Apply boundary also runs again inside the pipeline. Refusal here
        // avoids switching to a run screen for an already-known invalid plan.
        if let Err(error) = crate::pipeline::enforce_apply_boundary(&plan, false) {
            tracing::error!(%error, "TUI cleanup refused");
            self.status = Some((error.to_string(), true));
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.run = Some(Run {
            actions: vec![RunAction::Pending; plan.actions.len()],
            plan: plan.clone(),
            stop: stop.clone(),
            confirm_stop: false,
            output: VecDeque::new(),
            output_scroll: 0,
            scroll: 0,
            summary: None,
            error: None,
        });
        self.screens.push(Screen::Run);
        self.effects.push(Request::Execute {
            plan,
            home: self.home.clone(),
            stop,
        });
    }
    fn return_after_run(&mut self) {
        // Invalidate every overlapping arena (including ancestors), otherwise
        // descending through an old snapshot could resurrect removed bytes.
        let Some(run) = &self.run else {
            return;
        };
        let affected: Vec<_> = run
            .plan
            .actions
            .iter()
            .filter_map(|a| a.target.path.clone())
            .collect();
        let scope = run.plan.scope.clone();
        self.targets_cache.retain(|p, _| {
            p != &scope
                && !affected
                    .iter()
                    .any(|a| p.starts_with(a) || a.starts_with(p))
        });
        self.revision += 1;
        let stale = |p: &PathBuf| {
            affected
                .iter()
                .any(|a| p.starts_with(a) || a.starts_with(p))
        };
        self.cache.retain(|p, _| !stale(p));
        let invalid: Vec<_> = self
            .directories
            .keys()
            .filter(|p| stale(p))
            .cloned()
            .collect();
        for path in invalid {
            self.directories
                .insert(path.clone(), Directory::empty(self.revision));
            self.effects.push(Request::Rescan {
                path,
                revision: self.revision,
            });
        }
        self.ensure_current();
        self.reconcile_cursor();
    }
    fn on_cleanup_worker(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Planning {
                scope,
                generation,
                activity,
            } => {
                if let Some(targets) = self
                    .targets_cache
                    .get_mut(&scope)
                    .filter(|t| t.generation == generation)
                {
                    targets.activity = activity;
                }
            }
            WorkerEvent::Planned {
                scope,
                generation,
                purpose,
                result,
            } => {
                let Some(targets) = self
                    .targets_cache
                    .get_mut(&scope)
                    .filter(|t| t.generation == generation)
                else {
                    return;
                };
                targets.planning = false;
                match result {
                    Ok(plan) => {
                        if purpose == PlanPurpose::Detect {
                            targets.selected = plan
                                .targets
                                .iter()
                                .filter(|r| matches!(r.selection, PlanSelection::Selected { .. }))
                                .map(|r| r.candidate_key.clone())
                                .collect();
                            if let Some((selected, known)) = targets.previous_ticks.take() {
                                targets.selected = plan
                                    .targets
                                    .iter()
                                    .filter(|r| {
                                        r.target.manually_selectable()
                                            && if known.contains(&r.candidate_key) {
                                                selected.contains(&r.candidate_key)
                                            } else {
                                                matches!(
                                                    r.selection,
                                                    PlanSelection::Selected { .. }
                                                )
                                            }
                                    })
                                    .map(|r| r.candidate_key.clone())
                                    .collect();
                            }
                            targets.baseline = Some(plan.clone());
                            targets.finished = plan.scan_finished.unwrap_or_else(Instant::now);
                        }
                        targets.summary = targets.baseline.as_ref().map(|baseline| {
                            baseline.accounting.summarize(
                                baseline
                                    .targets
                                    .iter()
                                    .filter(|r| targets.selected.contains(&r.candidate_key)),
                            )
                        });
                        if purpose == PlanPurpose::Review
                            && self.screen() == Screen::Targets
                            && self.target_scope.as_ref() == Some(&scope)
                        {
                            // Only the returned plan becomes reviewable. The UI
                            // tick set cannot add an action to this handoff.
                            let kept: HashSet<_> = plan
                                .targets
                                .iter()
                                .filter(|r| matches!(r.selection, PlanSelection::Selected { .. }))
                                .map(|r| r.candidate_key.clone())
                                .collect();
                            let targets = self.targets_mut();
                            let mut dropped: Vec<_> = targets
                                .selected
                                .difference(&kept)
                                .map(|key| {
                                    targets
                                        .baseline
                                        .as_ref()
                                        .and_then(|p| {
                                            p.targets.iter().find(|r| &r.candidate_key == key)
                                        })
                                        .map_or_else(
                                            || key.to_string(),
                                            |r| {
                                                r.target.path.as_ref().map_or_else(
                                                    || r.target.id.clone(),
                                                    |p| p.display().to_string(),
                                                )
                                            },
                                        )
                                })
                                .collect();
                            dropped.sort();
                            targets.selected = kept;
                            targets.summary =
                                Some(plan.accounting.summarize(plan.targets.iter().filter(|r| {
                                    matches!(r.selection, PlanSelection::Selected { .. })
                                })));
                            self.review_notice = (!dropped.is_empty()).then(|| format!("⚠ {} selected items changed since detection and were dropped: {} · press r in targets to refresh", dropped.len(), dropped.join(", ")));
                            if plan.actions.is_empty() {
                                self.status = Some((
                                    self.review_notice.clone().unwrap_or_else(|| {
                                        "The rebuilt plan selects no executable actions".into()
                                    }),
                                    false,
                                ));
                            } else {
                                self.reviewed = Some(plan);
                                self.review_scroll = 0;
                                self.screens.push(Screen::Review);
                            }
                        }
                        if self.target_scope.as_ref() == Some(&scope) {
                            self.focus_target_path();
                            self.reconcile_target_cursor();
                        }
                    }
                    Err(message) => {
                        tracing::error!(%message, "TUI planning failed");
                        self.status = Some((message, true));
                    }
                }
            }
            WorkerEvent::Run { run_id, event } => {
                let Some(run) = self.run.as_mut().filter(|r| r.plan.run_id == run_id) else {
                    return;
                };
                match event {
                    RunEvent::Started { total } => debug_assert_eq!(total, run.actions.len()),
                    RunEvent::ActionStarted { index } => {
                        if let Some(a) = run.actions.get_mut(index) {
                            *a = RunAction::Running;
                        }
                    }
                    RunEvent::ActionFinished { index, result } => {
                        if let Some(a) = run.actions.get_mut(index) {
                            *a = RunAction::Finished(result);
                        }
                    }
                    RunEvent::Output {
                        index,
                        stream,
                        line,
                    } => {
                        for line in line.lines() {
                            if run.output.len() == 200 {
                                run.output.pop_front();
                            }
                            run.output.push_back(format!(
                                "{} {stream:?}: {}",
                                index + 1,
                                line.chars().filter(|c| !c.is_control()).collect::<String>()
                            ));
                        }
                    }
                    RunEvent::Finished(summary) => {
                        run.summary = Some(summary);
                        run.confirm_stop = false;
                        run.scroll = 0;
                        self.screens.retain(|s| *s != Screen::Help);
                        self.screens.pop();
                        self.screens.push(Screen::Results);
                    }
                    RunEvent::Error(message) => {
                        tracing::error!(%message, "TUI execution failed");
                        run.error = Some(message.clone());
                        self.status = Some((message, true));
                        self.screens.retain(|s| *s != Screen::Help);
                        self.screens.pop();
                        self.screens.push(Screen::Results);
                    }
                }
            }
            _ => unreachable!("browser events handled separately"),
        }
        self.trim_caches();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::worker::Summary;
    use cleanrs_core::fs::{Arena, Node, NodeFlags, NodeKind, ScanResult};

    pub(super) fn row(root: &str, name: &str, kind: NodeKind, bytes: u64, pending: bool) -> Row {
        Row {
            path: PathBuf::from(root).join(name),
            name: name.into(),
            kind,
            modified: bytes as i64,
            own_flags: NodeFlags::empty(),
            summary: Summary {
                allocated: bytes,
                apparent: bytes,
                ..Summary::default()
            },
            pending,
            source: None,
            estimate: None,
        }
    }
    pub(super) fn listed(path: &str, rows: Vec<Row>) -> WorkerEvent {
        WorkerEvent::Listed {
            path: path.into(),
            revision: 0,
            total: rows.iter().map(|r| r.summary.allocated).sum(),
            rows,
            volume: "Free 42.0 GiB · APFS".into(),
        }
    }
    pub(super) fn snapshot(path: &str, bytes: u64) -> Arc<Snapshot> {
        let mut arena = Arena::new(path);
        let mut directory = Node::default();
        directory.kind = NodeKind::Directory;
        let nested = arena.push_child(arena.root(), "nested", directory);
        let mut file = Node::default();
        file.kind = NodeKind::File;
        file.apparent_size = bytes;
        file.allocated_size = bytes;
        file.private_size = Some(bytes);
        file.device = 1;
        file.inode = 10;
        file.link_count = 1;
        arena.push_child(nested, "payload", file);
        Arc::new(Snapshot::new(
            ScanResult {
                root: path.into(),
                arena,
                coverage_errors: Vec::new(),
                backend: "test".into(),
                dataless_entries: 0,
                mount_boundaries: 0,
            },
            "Free 42.0 GiB · APFS".into(),
        ))
    }
    fn fixture() -> App {
        let mut app = App::new("/fixture".into());
        app.drain_requests();
        app.on_worker(listed(
            "/fixture",
            vec![
                row("/fixture", "alpha", NodeKind::Directory, 10, true),
                row("/fixture", "beta", NodeKind::Directory, 20, true),
                row("/fixture", "file", NodeKind::File, 5, false),
            ],
        ));
        app
    }

    #[test]
    fn navigation_before_scan_finishes_and_history_and_on_demand_parent() {
        let mut app = fixture();
        assert_eq!(app.highlighted().unwrap().name, "beta");
        app.handle(Action::Open);
        assert_eq!(app.current, PathBuf::from("/fixture/beta"));
        assert!(matches!(app.drain_requests()[0], Request::Browse { .. }));
        app.on_worker(listed(
            "/fixture/beta",
            vec![row("/fixture/beta", "x", NodeKind::File, 2, false)],
        ));
        app.handle(Action::Parent);
        assert_eq!(app.current, PathBuf::from("/fixture"));
        app.handle(Action::Previous);
        assert_eq!(app.current, PathBuf::from("/fixture/beta"));
        app.handle(Action::Parent);
        app.handle(Action::Parent);
        assert_eq!(app.current, PathBuf::from("/"));
        assert!(app.drain_requests().iter().any(
            |r| matches!(r, Request::Browse { path, .. } if path == PathBuf::from("/").as_path())
        ));
    }

    #[test]
    fn movement_sort_filter_help_targets_and_quit() {
        let mut app = fixture();
        app.handle(Action::Last);
        assert_eq!(app.highlighted().unwrap().name, "file");
        app.handle(Action::First);
        assert_eq!(app.highlighted().unwrap().name, "beta");
        app.page_rows = 4;
        app.handle(Action::HalfPage(1));
        assert_eq!(app.highlighted().unwrap().name, "file");
        app.handle(Action::HalfPage(-1));
        app.handle(Action::Sort);
        assert_eq!(app.sort, Sort::Name);
        assert_eq!(app.highlighted().unwrap().name, "beta");
        app.handle(Action::Sort);
        assert_eq!(app.sort, Sort::Age);
        app.handle(Action::Sort);
        assert_eq!(app.sort, Sort::Size);
        app.handle(Action::StartFilter);
        for c in "ALPH".chars() {
            app.handle(Action::FilterChar(c));
        }
        app.handle(Action::ConfirmFilter);
        assert_eq!(app.visible().len(), 1);
        assert_eq!(app.highlighted().unwrap().name, "alpha");
        app.handle(Action::ClearFilter);
        app.handle(Action::Match(-1));
        assert_eq!(app.highlighted().unwrap().name, "beta");
        app.handle(Action::Match(-1));
        assert_eq!(app.highlighted().unwrap().name, "file");
        app.handle(Action::Help);
        assert_eq!(app.screens, vec![Screen::Browser, Screen::Help]);
        app.handle(Action::Open);
        assert_eq!(app.current, PathBuf::from("/fixture"));
        app.handle(Action::Back);
        assert!(!app.quit);
        app.handle(Action::Targets);
        assert_eq!(app.screen(), Screen::Targets);
        app.handle(Action::Back);
        assert_eq!(app.screen(), Screen::Browser);
        app.handle(Action::Back);
        assert!(app.quit);
        assert!(matches!(app.drain_requests().last(), Some(Request::Quit)));
    }

    #[test]
    fn initial_cursor_follows_top_until_user_moves() {
        let mut app = fixture();
        // beta is initially first; alpha grows past it as scan results arrive.
        app.on_worker(WorkerEvent::ChildFinished {
            parent: "/fixture".into(),
            revision: 0,
            path: "/fixture/alpha".into(),
            snapshot: snapshot("/fixture/alpha", 100),
            total: 125,
        });
        assert_eq!(app.highlighted().unwrap().name, "alpha");
        app.handle(Action::Move(1));
        assert_eq!(app.highlighted().unwrap().name, "beta");
        app.on_worker(WorkerEvent::ChildFinished {
            parent: "/fixture".into(),
            revision: 0,
            path: "/fixture/beta".into(),
            snapshot: snapshot("/fixture/beta", 200),
            total: 305,
        });
        assert_eq!(app.highlighted().unwrap().name, "beta");
    }

    #[test]
    fn live_sort_keeps_identity_and_completed_subtrees_open_without_io() {
        let mut app = fixture();
        app.handle(Action::Move(1));
        assert_eq!(app.highlighted().unwrap().name, "alpha");
        let snapshot = snapshot("/fixture/alpha", 100);
        app.on_worker(WorkerEvent::ChildFinished {
            parent: "/fixture".into(),
            revision: 0,
            path: "/fixture/alpha".into(),
            snapshot,
            total: 125,
        });
        assert_eq!(app.visible()[0].name, "alpha");
        assert_eq!(app.highlighted().unwrap().name, "alpha");
        app.handle(Action::Open);
        assert_eq!(app.current, PathBuf::from("/fixture/alpha"));
        assert_eq!(app.highlighted().unwrap().name, "nested");
        assert!(app.drain_requests().is_empty());
        app.handle(Action::Open);
        assert_eq!(app.highlighted().unwrap().name, "payload");
        assert!(app.drain_requests().is_empty());
    }

    #[test]
    fn rescan_invalidates_overlapping_cache_and_rejects_stale_events() {
        let mut app = fixture();
        app.on_worker(WorkerEvent::ChildFinished {
            parent: "/fixture".into(),
            revision: 0,
            path: "/fixture/beta".into(),
            snapshot: snapshot("/fixture/beta", 100),
            total: 115,
        });
        app.handle(Action::Open);
        app.handle(Action::Rescan);
        assert!(app.cache.is_empty());
        assert!(matches!(
            app.drain_requests()[0],
            Request::Rescan { revision: 1, .. }
        ));
        app.on_worker(listed("/fixture/beta", Vec::new()));
        assert!(!app.directory().listed);
        app.on_worker(WorkerEvent::Listed {
            path: app.current.clone(),
            revision: 1,
            rows: Vec::new(),
            total: 0,
            volume: "unknown".into(),
        });
        assert!(!app.busy());
        app.handle(Action::Quit);
        assert!(app.quit);
    }

    #[test]
    fn subtree_warning_does_not_prevent_opening_safe_ancestor() {
        let mut row = row("/fixture", "safe", NodeKind::Directory, 10, false);
        row.summary.flags = NodeFlags::DATALESS | NodeFlags::MOUNT_BOUNDARY;
        assert!(row.navigable());
        row.own_flags = NodeFlags::DATALESS;
        assert!(!row.navigable());
    }
    #[test]
    fn cache_eviction_releases_all_aliases_and_inactive_row_sources() {
        let mut app = App::new("/fixture".into());
        app.drain_requests();
        let source = snapshot("/fixture/large", 42);
        for n in 0..=CACHE_PATHS {
            app.cache.insert(
                PathBuf::from(format!("/alias/{n}")),
                (source.clone(), source.scan.arena.root()),
            );
        }
        let mut inactive = Directory::empty(0);
        let mut retained = row("/fixture", "large", NodeKind::Directory, 42, false);
        retained.source = Some((source.clone(), source.scan.arena.root()));
        inactive.rows.push(retained);
        app.directories.insert("/inactive".into(), inactive);
        let weak = Arc::downgrade(&source);
        drop(source);
        app.trim_caches();
        assert!(app.cache.is_empty());
        assert!(!app.directories.contains_key(&PathBuf::from("/inactive")));
        assert!(
            weak.upgrade().is_none(),
            "eviction must release the shared arena"
        );
        for n in 0..CACHE_LISTINGS + 10 {
            app.directories
                .insert(PathBuf::from(format!("/listing/{n}")), Directory::empty(0));
        }
        for n in 0..CACHE_TARGET_SCOPES + 10 {
            app.targets_cache
                .insert(PathBuf::from(format!("/targets/{n}")), Targets::default());
        }
        app.trim_caches();
        assert_eq!(app.directories.len(), CACHE_LISTINGS);
        assert_eq!(app.targets_cache.len(), CACHE_TARGET_SCOPES);
        assert!(app.directories.contains_key(&app.current));
    }
}
