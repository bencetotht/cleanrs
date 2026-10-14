//! Bounded child scans, cooperative cancellation, and read-only accounting jobs.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::cache::DiskCache;
use crate::pipeline::{
    self, ExecutionEvent, ExecutionObserver, PlanEvent, PlanObserver, PlanRequest, ReviewedPlan,
};
use cleanrs_core::accounting::{ReclaimEstimate, estimate_subtree};
use cleanrs_core::execution::{ActionResult, OutputStream};
use cleanrs_core::fs::{
    Arena, EntryMetadata, FileSystem, NativeFileSystem, Node, NodeFlags, NodeIndex, NodeKind,
    ScanObserver, ScanOptions, ScanProgress, ScanResult, Scanner,
};
use cleanrs_core::journal::JournalRun;
use cleanrs_core::targets::{BUILTIN_TARGETS, CandidateKey, CleanupLevel, SelectionMode};

/// Each request is tagged so late plans cannot replace a newer tick set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanPurpose {
    Detect,
    Review,
}

/// Matches the CLI defaults, including its fixed user config location. The
/// TUI offers no ad-hoc targets and never resolves config from the scan root.
pub fn plan_request(
    path: PathBuf,
    home: Option<PathBuf>,
    level: CleanupLevel,
    machine: bool,
    selected: Option<HashSet<CandidateKey>>,
) -> PlanRequest {
    let manual = selected.is_some();
    PlanRequest {
        path,
        home,
        level,
        explicitly_named_ids: Vec::new(),
        only: Vec::new(),
        exclude: Vec::new(),
        ignore_staleness: false,
        workers: None,
        workspace_only: !machine,
        cargo_stale_after: 14,
        config: None,
        no_builtin_targets: false,
        target: Vec::new(),
        target_argv: Vec::new(),
        targets_only_cli: false,
        manual_selections: selected.unwrap_or_default(),
        select: Vec::new(),
        selection_mode: if manual {
            SelectionMode::ManualOnly
        } else {
            SelectionMode::LevelPlusManual
        },
    }
}

#[derive(Debug)]
pub enum RunEvent {
    Started {
        total: usize,
    },
    ActionStarted {
        index: usize,
    },
    Output {
        index: usize,
        stream: OutputStream,
        line: String,
    },
    ActionFinished {
        index: usize,
        result: ActionResult,
    },
    Finished(JournalRun),
    Error(String),
}

use crossbeam_channel::{Receiver, Sender};

type Inode = (u64, u64);

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub allocated: u64,
    pub apparent: u64,
    pub targets: usize,
    pub target_id: Option<String>,
    pub flags: NodeFlags,
    pub incomplete: bool,
    pub measured: bool,
}

/// One owned arena is shared by all cached descendants, rather than copied.
#[derive(Debug)]
pub struct Snapshot {
    pub scan: ScanResult,
    pub summaries: Vec<Summary>,
    pub directories: HashMap<PathBuf, NodeIndex>,
    pub finished: Instant,
    pub age_offset: Duration,
    pub volume: String,
    pub cached: bool,
}

impl Snapshot {
    pub(super) fn new(scan: ScanResult, volume: String) -> Self {
        let arena = &scan.arena;
        let mut summaries = vec![Summary::default(); arena.nodes.len()];
        let mut inodes: Vec<HashMap<Inode, u64>> = vec![HashMap::new(); arena.nodes.len()];
        let mut directories = HashMap::new();
        let errors: HashSet<_> = scan
            .coverage_errors
            .iter()
            .map(|error| error.path.clone())
            .collect();
        // Scanner parents always precede children. Merge inode sets bottom-up
        // (small into large) to deduplicate hardlinks at every directory scope.
        for position in (0..arena.nodes.len()).rev() {
            let index = NodeIndex::from_usize(position);
            let node = &arena.nodes[position];
            let path = arena.path(index);
            let summary = &mut summaries[position];
            summary.measured = true;
            summary.flags |= node.flags;
            summary.incomplete |= errors.contains(&path);
            if node.kind.is_file_like() {
                summary.apparent = node.apparent_size;
                inodes[position].insert((node.device, node.inode), node.allocated_size);
            }
            if node.kind == NodeKind::Directory {
                directories.insert(path, index);
                let children: HashMap<_, _> = arena
                    .children(index)
                    .map(|i| (arena.name(i), arena.nodes[i.as_usize()].kind))
                    .collect();
                summary.target_id = arena.parent(index).and_then(|parent| {
                    BUILTIN_TARGETS
                        .iter()
                        .find(|target| {
                            arena.name(index) == target.detector.directory
                                && arena
                                    .children(parent)
                                    .any(|child| arena.name(child) == target.detector.marker)
                        })
                        .map(|target| target.id.to_owned())
                });
                summary.targets += BUILTIN_TARGETS
                    .iter()
                    .filter(|target| {
                        children.contains_key(target.detector.marker)
                            && children.get(target.detector.directory) == Some(&NodeKind::Directory)
                    })
                    .count();
            }
            summary.allocated = inodes[position].values().copied().sum();
            if let Some(parent) = arena.parent(index) {
                let parent = parent.as_usize();
                let child = summaries[position].clone();
                summaries[parent].apparent =
                    summaries[parent].apparent.saturating_add(child.apparent);
                summaries[parent].targets += child.targets;
                summaries[parent].flags |= child.flags;
                summaries[parent].incomplete |= child.incomplete;
                let mut child_inodes = std::mem::take(&mut inodes[position]);
                if child_inodes.len() > inodes[parent].len() {
                    std::mem::swap(&mut child_inodes, &mut inodes[parent]);
                }
                inodes[parent].extend(child_inodes);
            }
        }
        // An enumeration error can refer to a vanished child absent from the
        // arena. Mark its closest recorded ancestor and propagate upwards.
        for error in &scan.coverage_errors {
            let mut path = error.path.as_path();
            while !directories.contains_key(path) {
                let Some(parent) = path.parent() else {
                    break;
                };
                path = parent;
            }
            let mut cursor = directories.get(path).copied();
            while let Some(index) = cursor {
                summaries[index.as_usize()].incomplete = true;
                cursor = arena.parent(index);
            }
        }
        Self {
            scan,
            summaries,
            directories,
            finished: Instant::now(),
            age_offset: Duration::ZERO,
            volume,
            cached: false,
        }
    }

    /// Cached age may predate the current machine's monotonic clock (reboot).
    pub fn age(&self) -> Duration {
        self.age_offset.saturating_add(self.finished.elapsed())
    }

    pub fn rows(self: &Arc<Self>, root: NodeIndex) -> Vec<Row> {
        self.scan
            .arena
            .children(root)
            .map(|index| {
                let node = &self.scan.arena.nodes[index.as_usize()];
                Row {
                    path: self.scan.arena.path(index),
                    name: self.scan.arena.name(index).to_owned(),
                    kind: node.kind,
                    modified: node.modified_unix_seconds,
                    own_flags: node.flags,
                    summary: self.summaries[index.as_usize()].clone(),
                    pending: false,
                    source: Some((Arc::clone(self), index)),
                    estimate: None,
                }
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub kind: NodeKind,
    pub modified: i64,
    pub own_flags: NodeFlags,
    pub summary: Summary,
    pub pending: bool,
    pub source: Option<(Arc<Snapshot>, NodeIndex)>,
    pub estimate: Option<ReclaimEstimate>,
}
impl Row {
    pub fn navigable(&self) -> bool {
        self.kind == NodeKind::Directory
            && !self
                .own_flags
                .intersects(NodeFlags::MOUNT_BOUNDARY | NodeFlags::DATALESS | NodeFlags::FIRMLINK)
    }
}

/// Completed browser arenas supplied to discovery; shallow files are refreshed
/// on the worker. A missing/new directory falls back to normal scanning.
pub struct BrowserScan {
    pub sources: Vec<(Arc<Snapshot>, NodeIndex)>,
}
impl BrowserScan {
    fn assemble(self, path: &Path) -> std::io::Result<Option<ScanResult>> {
        let filesystem = NativeFileSystem;
        let root = filesystem.metadata(path)?;
        if root.kind != NodeKind::Directory
            || root
                .flags
                .intersects(NodeFlags::DATALESS | NodeFlags::FIRMLINK)
        {
            return Ok(None);
        }
        let mut arena = Arena::new(path);
        arena.update_metadata(arena.root(), node_from_metadata(&root));
        let mut errors = Vec::new();
        let entries = filesystem.read_dir(path)?;
        for entry in entries {
            let child = path.join(&entry.name);
            let mut node = node_from_metadata(&entry.metadata);
            if node.device != root.device {
                node.flags.insert(NodeFlags::MOUNT_BOUNDARY);
            }
            let traversable = node.kind == NodeKind::Directory
                && !node.flags.intersects(
                    NodeFlags::DATALESS | NodeFlags::FIRMLINK | NodeFlags::MOUNT_BOUNDARY,
                );
            let index = arena.push_child(arena.root(), &entry.name.to_string_lossy(), node);
            if traversable {
                let Some((snapshot, source)) = self
                    .sources
                    .iter()
                    .find(|(s, i)| s.scan.arena.path(*i) == child)
                else {
                    return Ok(None);
                };
                let old = &snapshot.scan.arena.nodes[source.as_usize()];
                if (old.device, old.inode) != (entry.metadata.device, entry.metadata.inode) {
                    return Ok(None);
                }
                snapshot
                    .scan
                    .arena
                    .copy_children_into(*source, &mut arena, index);
                errors.extend(
                    snapshot
                        .scan
                        .coverage_errors
                        .iter()
                        .filter(|e| e.path.starts_with(&child))
                        .cloned(),
                );
            }
        }
        let dataless_entries = arena
            .nodes
            .iter()
            .filter(|n| n.flags.contains(NodeFlags::DATALESS))
            .count();
        let mount_boundaries = arena
            .nodes
            .iter()
            .filter(|n| n.flags.contains(NodeFlags::MOUNT_BOUNDARY))
            .count();
        Ok(Some(ScanResult {
            root: path.into(),
            arena,
            coverage_errors: errors,
            backend: "browser cache".into(),
            dataless_entries,
            mount_boundaries,
        }))
    }
}

pub enum Request {
    Plan {
        generation: u64,
        purpose: PlanPurpose,
        request: Box<PlanRequest>,
        detection: Option<Arc<ReviewedPlan>>,
        browser_scan: Option<BrowserScan>,
    },
    Execute {
        plan: Arc<ReviewedPlan>,
        home: Option<PathBuf>,
        stop: Arc<AtomicBool>,
    },
    Browse {
        path: PathBuf,
        revision: u64,
    },
    Rescan {
        path: PathBuf,
        revision: u64,
    },
    Explain {
        path: PathBuf,
        revision: u64,
        snapshot: Arc<Snapshot>,
        index: NodeIndex,
    },
    Quit,
}

#[derive(Debug)]
pub enum WorkerEvent {
    Planning {
        scope: PathBuf,
        generation: u64,
        activity: String,
    },
    Planned {
        scope: PathBuf,
        generation: u64,
        purpose: PlanPurpose,
        result: Result<Arc<ReviewedPlan>, String>,
    },
    Run {
        run_id: String,
        event: RunEvent,
    },
    Restored {
        path: PathBuf,
        revision: u64,
        snapshot: Arc<Snapshot>,
    },
    Listed {
        path: PathBuf,
        revision: u64,
        rows: Vec<Row>,
        total: u64,
        volume: String,
    },
    Progress {
        parent: PathBuf,
        revision: u64,
        child: PathBuf,
        progress: ScanProgress,
    },
    ChildFinished {
        parent: PathBuf,
        revision: u64,
        path: PathBuf,
        snapshot: Arc<Snapshot>,
        total: u64,
    },
    Explained {
        parent: PathBuf,
        revision: u64,
        path: PathBuf,
        estimate: ReclaimEstimate,
    },
    CacheWarning {
        parent: PathBuf,
        revision: u64,
        message: String,
    },
    Coverage {
        parent: PathBuf,
        revision: u64,
    },
    Error {
        parent: PathBuf,
        revision: u64,
        child: Option<PathBuf>,
        message: String,
    },
}

struct ScanJob {
    parent: PathBuf,
    revision: u64,
    path: PathBuf,
    cancellation: Arc<AtomicBool>,
    volume: String,
    cache: Option<DiskCache>,
    allow_cache: bool,
}
enum Job {
    Scan(ScanJob),
    Explain {
        parent: PathBuf,
        revision: u64,
        snapshot: Arc<Snapshot>,
        index: NodeIndex,
    },
}
struct Finished {
    job: ScanJob,
    result: Result<Arc<Snapshot>, String>,
}
struct DirectoryJob {
    remaining: usize,
    revision: u64,
    inodes: HashMap<Inode, u64>,
    cancellation: Arc<AtomicBool>,
}

/// The coordinator owns queue priority and cancellation; two scanner jobs with
/// one scanner worker each bound traversal concurrency independently of trees.
pub struct Workers {
    pub requests: Sender<Request>,
    pub events: Receiver<WorkerEvent>,
    handle: Option<JoinHandle<()>>,
}
impl Workers {
    #[cfg(test)]
    pub fn start() -> Self {
        Self::start_with_cache(None)
    }
    pub fn start_with_cache(cache: Option<DiskCache>) -> Self {
        let (requests, input) = crossbeam_channel::unbounded();
        // Backpressure bounds streamed provider output before it reaches the
        // App's last-200-line pane. No output path can grow an unbounded queue.
        let (output, events) = crossbeam_channel::bounded(256);
        let handle = std::thread::spawn(move || coordinate(input, output, cache));
        Self {
            requests,
            events,
            handle: Some(handle),
        }
    }
}
impl Drop for Workers {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Quit);
        // Do not block terminal restoration on an OS syscall in flight. The
        // coordinator cancels every token and joins its pool off the UI thread.
        self.handle.take();
    }
}

fn coordinate(input: Receiver<Request>, output: Sender<WorkerEvent>, cache: Option<DiskCache>) {
    let (cleanup, cleanup_input) = crossbeam_channel::unbounded();
    let cleanup_output = output.clone();
    let cleanup_cache = cache.clone();
    let cleanup_handle =
        std::thread::spawn(move || cleanup_jobs(cleanup_input, cleanup_output, cleanup_cache));
    let (jobs, queue) = crossbeam_channel::bounded::<Job>(2);
    let (finished_tx, finished) = crossbeam_channel::unbounded::<Finished>();
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let queue = queue.clone();
            let finished = finished_tx.clone();
            let output = output.clone();
            std::thread::spawn(move || {
                while let Ok(job) = queue.recv() {
                    match job {
                        Job::Scan(job) => {
                            let result = scan_child(&job, &output);
                            if finished.send(Finished { job, result }).is_err() {
                                break;
                            }
                        }
                        Job::Explain {
                            parent,
                            revision,
                            snapshot,
                            index,
                        } => {
                            let estimate = estimate_subtree(
                                &snapshot.scan.arena,
                                index,
                                !snapshot.summaries[index.as_usize()].incomplete,
                            );
                            let _ = output.send(WorkerEvent::Explained {
                                parent,
                                revision,
                                path: snapshot.scan.arena.path(index),
                                estimate,
                            });
                        }
                    }
                }
            })
        })
        .collect();
    let mut pending = VecDeque::new();
    let mut directories: HashMap<PathBuf, DirectoryJob> = HashMap::new();
    let mut active = 0;
    loop {
        while active < 2 {
            let Some(job) = pending.pop_front() else {
                break;
            };
            if matches!(&job, Job::Scan(job) if job.cancellation.load(Ordering::Acquire)) {
                continue;
            }
            // Explain jobs don't emit Finished; retain scan slots by dispatching
            // their small accounting task here on the coordinator instead.
            if let Job::Explain {
                parent,
                revision,
                snapshot,
                index,
            } = job
            {
                let estimate = estimate_subtree(
                    &snapshot.scan.arena,
                    index,
                    !snapshot.summaries[index.as_usize()].incomplete,
                );
                let _ = output.send(WorkerEvent::Explained {
                    parent,
                    revision,
                    path: snapshot.scan.arena.path(index),
                    estimate,
                });
            } else {
                if jobs.send(job).is_err() {
                    break;
                }
                active += 1;
            }
        }
        crossbeam_channel::select! {
            recv(input) -> request => {
                match request {
                    Ok(request @ (Request::Browse { .. } | Request::Rescan { .. })) => {
                        let refresh = matches!(request, Request::Rescan { .. });
                        let (path, revision) = match request { Request::Browse { path, revision } | Request::Rescan { path, revision } => (path, revision), _ => unreachable!() };

                        // A newer revision cancels overlapping ancestor jobs too:
                        // otherwise their old arena could resurrect stale data.
                        let overlapping: Vec<_> = directories.iter().filter(|(p, d)| d.revision < revision && (p.starts_with(&path) || path.starts_with(p))).map(|(p, _)| p.clone()).collect();
                        for p in overlapping {
                            if let Some(job) = directories.remove(&p) { job.cancellation.store(true, Ordering::Release); }
                        }
                        if refresh {
                            if let Some(cache) = &cache {
                                if let Err(error) = cache.invalidate(&path) { tracing::warn!(%error, "cannot invalidate browser disk cache"); }
                            }
                        }
                        // Normal navigation uses the same revision and does not
                        // cancel work; move queued jobs for the visible scope up.
                        let mut prioritized = VecDeque::new();
                        let mut rest = VecDeque::new();
                        for job in pending.drain(..) {
                            if matches!(&job, Job::Scan(j) if j.path.starts_with(&path)) { prioritized.push_back(job); } else { rest.push_back(job); }
                        }
                        prioritized.append(&mut rest);
                        pending = prioritized;
                        if directories.get(&path).is_some_and(|d| d.revision == revision) { continue; }
                        if !refresh {
                            if let Some(cache) = &cache {
                                match cache.load_tree(&path) {
                                    Ok(Some((scan, age))) => {
                                        let restored_at = Instant::now();
                                        let mut snapshot = Snapshot::new(scan, volume_label(&path));
                                        snapshot.finished = restored_at;
                                        snapshot.age_offset = age;
                                        snapshot.cached = true;
                                        let _ = output.send(WorkerEvent::Restored { path, revision, snapshot: Arc::new(snapshot) });
                                        continue;
                                    }
                                    Ok(None) => {}
                                    Err(error) => tracing::warn!(%error, "cannot restore cached browser scope"),
                                }
                            }
                        }
                        match list_children(&path, revision, &output) {
                            Ok((directory, mut child_jobs)) => {
                                for job in &mut child_jobs { job.cache = cache.clone(); job.allow_cache = !refresh; }
                                if directory.remaining > 0 { directories.insert(path, directory); }
                                for job in child_jobs.into_iter().rev() { pending.push_front(Job::Scan(job)); }
                            }
                            Err(error) => report_io_error(&output, path, revision, error),
                        }
                    }
                    Ok(Request::Explain { path, revision, snapshot, index }) => pending.push_front(Job::Explain { parent: path, revision, snapshot, index }),
                    Ok(request @ Request::Plan { .. }) => { let _ = cleanup.send(request); }
                    Ok(request @ Request::Execute { .. }) => {
                        // Freeze browser facts overlapping mutation before cache
                        // invalidation. Their cancelled writers cannot publish old
                        // arenas after the cleanup worker removes those entries.
                        if let Request::Execute { plan, .. } = &request {
                            let paths: Vec<_> = plan.actions.iter().filter_map(|a| a.target.path.as_ref()).collect();
                            let affected: Vec<_> = directories.keys().filter(|p| paths.iter().any(|a| p.starts_with(a) || a.starts_with(p))).cloned().collect();
                            for path in affected {
                                if let Some(job) = directories.remove(&path) { job.cancellation.store(true, Ordering::Release); }
                            }
                        }
                        let _ = cleanup.send(request);
                    }
                    Ok(Request::Quit) | Err(_) => break,
                }
            }
            recv(finished) -> result => {
                let Ok(Finished { job, result }) = result else { break; };
                active -= 1;
                if job.cancellation.load(Ordering::Acquire) { continue; }
                if let Some(directory) = directories.get_mut(&job.parent).filter(|d| d.revision == job.revision) {
                    match result {
                        Ok(snapshot) => {
                            for node in &snapshot.scan.arena.nodes {
                                if node.kind.is_file_like() { directory.inodes.insert((node.device, node.inode), node.allocated_size); }
                            }
                            let total = directory.inodes.values().copied().sum();
                            let _ = output.send(WorkerEvent::ChildFinished { parent: job.parent.clone(), revision: job.revision, path: job.path, snapshot, total });
                        }
                        Err(message) => report_error(&output, job.parent.clone(), job.revision, Some(job.path), message),
                    }
                    directory.remaining = directory.remaining.saturating_sub(1);
                    if directory.remaining == 0 { directories.remove(&job.parent); }
                }
            }
        }
    }
    for directory in directories.values() {
        directory.cancellation.store(true, Ordering::Release);
    }
    let _ = cleanup.send(Request::Quit);
    let _ = cleanup_handle.join();
    drop(jobs);
    for handle in handles {
        let _ = handle.join();
    }
}

/// Serialize planning/execution on a dedicated worker. Coalesce queued plans
/// before scanning, but never discard an execution request or interrupt an
/// action in progress. Selection changes never enter this queue.
fn cleanup_jobs(input: Receiver<Request>, output: Sender<WorkerEvent>, cache: Option<DiskCache>) {
    let mut pending = VecDeque::new();
    while let Some(mut request) = pending.pop_front().or_else(|| input.recv().ok()) {
        if let Request::Plan { request: plan, .. } = &request {
            let scope = plan.path.clone();
            while let Ok(next) = input.try_recv() {
                if matches!(&next, Request::Plan { request, .. } if request.path == scope) {
                    request = next;
                } else {
                    pending.push_back(next);
                    break;
                }
            }
        }
        match request {
            Request::Plan {
                generation,
                purpose,
                request,
                detection,
                browser_scan,
            } => {
                let scope = request.path.clone();
                let observer = Arc::new(PlanForwarder {
                    output: output.clone(),
                    scope: scope.clone(),
                    generation,
                    last: std::sync::Mutex::new(Instant::now()),
                });
                let result = if purpose == PlanPurpose::Review {
                    pipeline::build_tui_reviewed_plan(
                        *request,
                        observer,
                        detection
                            .as_deref()
                            .expect("review requires cached detection"),
                    )
                } else {
                    let (finished, age_offset) = browser_scan
                        .as_ref()
                        .and_then(|s| s.sources.iter().max_by_key(|(s, _)| s.age()))
                        .map_or_else(
                            || (Instant::now(), Duration::ZERO),
                            |(s, _)| (s.finished, s.age_offset),
                        );
                    match browser_scan
                        .map(|scan| scan.assemble(&request.path))
                        .transpose()
                    {
                        Ok(Some(Some(scan))) => pipeline::build_plan_from_scan(
                            *request, observer, scan, finished, age_offset,
                        ),
                        Ok(_) => pipeline::build_reviewed_plan(*request, observer),
                        Err(error) => Err(anyhow::Error::new(error).context(format!(
                            "failed to build cleanup plan in {}",
                            request.path.display()
                        ))),
                    }
                }
                .map(Arc::new)
                .map_err(|e| format!("{e:#}"));
                if let Err(message) = &result {
                    tracing::error!(%message, "TUI planning failed");
                }
                let _ = output.send(WorkerEvent::Planned {
                    scope,
                    generation,
                    purpose,
                    result,
                });
            }
            Request::Execute { plan, home, stop } => {
                let mut observer = ExecutionForwarder {
                    output: output.clone(),
                    run_id: plan.run_id.clone(),
                };
                if let Some(cache) = &cache {
                    for path in plan.actions.iter().filter_map(|a| a.target.path.as_ref()) {
                        if let Err(error) = cache.invalidate(path) {
                            tracing::warn!(%error, "cannot invalidate cleaned browser cache");
                        }
                    }
                }
                let result =
                    pipeline::journal_for_environment(home.as_deref()).and_then(|journal| {
                        // Interactive confirmation is explicit in App; retain the
                        // CLI root boundary and all immediate mutation checks.
                        pipeline::execute_reviewed_plan(
                            &plan,
                            home.as_deref(),
                            false,
                            &stop,
                            &mut observer,
                            &journal,
                        )
                    });
                if let Err(error) = result {
                    tracing::error!(%error, "TUI execution failed");
                    observer.send(RunEvent::Error(format!("{error:#}")));
                }
            }
            Request::Quit => break,
            _ => unreachable!("cleanup queue only accepts cleanup work"),
        }
    }
}
struct PlanForwarder {
    output: Sender<WorkerEvent>,
    scope: PathBuf,
    generation: u64,
    last: std::sync::Mutex<Instant>,
}
impl PlanForwarder {
    fn send(&self, activity: String) {
        let _ = self.output.send(WorkerEvent::Planning {
            scope: self.scope.clone(),
            generation: self.generation,
            activity,
        });
    }
}
impl PlanObserver for PlanForwarder {
    fn on_event(&self, event: PlanEvent) {
        self.send(match event {
            PlanEvent::PhaseStarted { phase } => format!("{phase}…"),
            PlanEvent::PhaseFinished { phase, count } => format!("✓ {phase} · {count} found"),
            PlanEvent::CargoMetadata { completed, total } => {
                format!("cargo metadata {completed}/{total} manifests")
            }
        });
    }
    fn on_scan_progress(&self, progress: ScanProgress) {
        let mut last = self.last.lock().unwrap();
        if last.elapsed().as_millis() >= 80 {
            *last = Instant::now();
            self.send(format!(
                "workspace detection · {} dirs · {} entries · {}",
                progress.directories,
                progress.entries,
                crate::human_bytes(progress.allocated_bytes)
            ));
        }
    }
}
struct ExecutionForwarder {
    output: Sender<WorkerEvent>,
    run_id: String,
}
impl ExecutionForwarder {
    fn send(&self, event: RunEvent) {
        let _ = self.output.send(WorkerEvent::Run {
            run_id: self.run_id.clone(),
            event,
        });
    }
}
impl ExecutionObserver for ExecutionForwarder {
    fn on_event(&mut self, event: ExecutionEvent<'_>) {
        self.send(match event {
            ExecutionEvent::RunStarted { total } => RunEvent::Started { total },
            ExecutionEvent::ActionStarted { index, .. } => RunEvent::ActionStarted { index },
            ExecutionEvent::OutputLine {
                index,
                stream,
                line,
            } => RunEvent::Output {
                index,
                stream,
                line: String::from_utf8_lossy(line).into_owned(),
            },
            ExecutionEvent::ActionFinished { index, result } => RunEvent::ActionFinished {
                index,
                result: result.clone(),
            },
            ExecutionEvent::RunFinished { summary } => RunEvent::Finished(summary.clone()),
        });
    }
}

fn report_error(
    output: &Sender<WorkerEvent>,
    parent: PathBuf,
    revision: u64,
    child: Option<PathBuf>,
    message: String,
) {
    tracing::error!(path = %parent.display(), %message, "TUI worker failed");
    let _ = output.send(WorkerEvent::Error {
        parent,
        revision,
        child,
        message,
    });
}

fn report_io_error(
    output: &Sender<WorkerEvent>,
    parent: PathBuf,
    revision: u64,
    error: std::io::Error,
) {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        tracing::warn!(path = %parent.display(), %error, "TUI incomplete coverage");
        let _ = output.send(WorkerEvent::Coverage { parent, revision });
    } else {
        report_error(output, parent, revision, None, error.to_string());
    }
}

fn list_children(
    path: &Path,
    revision: u64,
    output: &Sender<WorkerEvent>,
) -> std::io::Result<(DirectoryJob, Vec<ScanJob>)> {
    let filesystem = NativeFileSystem;
    let root = filesystem.metadata(path)?;
    if root.kind != NodeKind::Directory {
        return Err(std::io::Error::other(
            "browse root must be a directory (symlinks are not followed)",
        ));
    }
    if root
        .flags
        .intersects(NodeFlags::DATALESS | NodeFlags::FIRMLINK)
    {
        return Err(std::io::Error::other(
            "dataless/firmlink directory is not traversed",
        ));
    }
    let volume = volume_label(path);
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut inodes = HashMap::new();
    let mut rows = Vec::new();
    let mut jobs = Vec::new();
    // Use OS names to retain exact paths even when display strings are lossy.
    // Per-entry failures stay visible as coverage rows rather than losing the
    // entire listing when a child disappears between readdir and metadata.
    for entry in std::fs::read_dir(path)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report_io_error(output, path.into(), revision, error);
                continue;
            }
        };
        let child = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = match filesystem.metadata(&child) {
            Ok(metadata) => metadata,
            Err(error) => {
                report_io_error(output, path.into(), revision, error);
                rows.push(Row {
                    path: child,
                    name,
                    kind: NodeKind::Other,
                    modified: 0,
                    own_flags: NodeFlags::empty(),
                    summary: Summary {
                        incomplete: true,
                        ..Summary::default()
                    },
                    pending: false,
                    source: None,
                    estimate: None,
                });
                continue;
            }
        };
        let mut flags = metadata.flags;
        if metadata.device != root.device {
            flags.insert(NodeFlags::MOUNT_BOUNDARY);
        }
        let pending = metadata.kind == NodeKind::Directory
            && !flags
                .intersects(NodeFlags::MOUNT_BOUNDARY | NodeFlags::DATALESS | NodeFlags::FIRMLINK);
        let summary = Summary {
            allocated: if metadata.kind.is_file_like() {
                metadata.allocated_size
            } else {
                0
            },
            apparent: if metadata.kind.is_file_like() {
                metadata.apparent_size
            } else {
                0
            },
            flags,
            ..Summary::default()
        };
        if metadata.kind.is_file_like() {
            inodes.insert((metadata.device, metadata.inode), metadata.allocated_size);
        }
        let estimate = if metadata.kind.is_file_like() {
            let mut arena = Arena::new(path);
            let index = arena.push_child(arena.root(), &name, node_from_metadata(&metadata));
            Some(estimate_subtree(&arena, index, true))
        } else {
            None
        };
        rows.push(Row {
            path: child.clone(),
            name,
            kind: metadata.kind,
            modified: metadata.modified_unix_seconds,
            own_flags: flags,
            summary,
            pending,
            source: None,
            estimate,
        });
        if pending {
            jobs.push(ScanJob {
                parent: path.into(),
                revision,
                path: child,
                cancellation: Arc::clone(&cancellation),
                volume: volume.clone(),
                cache: None,
                allow_cache: true,
            });
        }
    }
    let names: HashSet<_> = rows.iter().map(|r| r.name.clone()).collect();
    for row in &mut rows {
        if row.kind == NodeKind::Directory {
            row.summary.target_id = BUILTIN_TARGETS
                .iter()
                .find(|t| row.name == t.detector.directory && names.contains(t.detector.marker))
                .map(|t| t.id.to_owned());
        }
    }
    let total = inodes.values().copied().sum();
    let _ = output.send(WorkerEvent::Listed {
        path: path.into(),
        revision,
        rows,
        total,
        volume,
    });
    Ok((
        DirectoryJob {
            remaining: jobs.len(),
            revision,
            inodes,
            cancellation,
        },
        jobs,
    ))
}

fn node_from_metadata(m: &EntryMetadata) -> Node {
    let mut node = Node::default();
    node.kind = m.kind;
    node.apparent_size = m.apparent_size;
    node.allocated_size = m.allocated_size;
    node.private_size = m.private_size;
    node.device = m.device;
    node.inode = m.inode;
    node.link_count = m.link_count;
    node.clone_id = m.clone_id;
    node.clone_refcount = m.clone_refcount;
    node.modified_unix_seconds = m.modified_unix_seconds;
    node.flags = m.flags;
    node
}

struct Observer {
    output: Sender<WorkerEvent>,
    parent: PathBuf,
    child: PathBuf,
    revision: u64,
    last: std::sync::Mutex<Instant>,
}
impl ScanObserver for Observer {
    fn on_progress(&self, progress: ScanProgress) {
        // Coalesce notifications at the source so large trees cannot fill the
        // UI event queue with one message per directory.
        let mut last = self.last.lock().unwrap();
        if last.elapsed().as_millis() < 80 {
            return;
        }
        *last = Instant::now();
        let _ = self.output.send(WorkerEvent::Progress {
            parent: self.parent.clone(),
            revision: self.revision,
            child: self.child.clone(),
            progress,
        });
    }
}
fn scan_child(job: &ScanJob, output: &Sender<WorkerEvent>) -> Result<Arc<Snapshot>, String> {
    if job.cancellation.load(Ordering::Acquire) {
        return Err("scan cancelled".into());
    }
    if job.allow_cache {
        if let Some(cache) = &job.cache {
            match cache.load_tree(&job.path) {
                Ok(Some((scan, age))) => {
                    let restored_at = Instant::now();
                    let mut snapshot = Snapshot::new(scan, job.volume.clone());
                    snapshot.finished = restored_at;
                    snapshot.age_offset = age;
                    snapshot.cached = true;
                    return Ok(Arc::new(snapshot));
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, path = %job.path.display(), "browser cache miss; scanning fresh")
                }
            }
        }
    }
    let observer = Arc::new(Observer {
        output: output.clone(),
        parent: job.parent.clone(),
        child: job.path.clone(),
        revision: job.revision,
        last: std::sync::Mutex::new(Instant::now()),
    });
    let scan = Scanner::new(ScanOptions {
        workers: 1,
        ..ScanOptions::default()
    })
    .scan_with_observer_cancellable(&job.path, observer, Some(Arc::clone(&job.cancellation)))
    .map_err(|e| e.to_string())?;
    if let Some(cache) = &job.cache {
        if let Err(error) = cache.save_if(&scan, || !job.cancellation.load(Ordering::Acquire)) {
            tracing::warn!(%error, "cannot save browser disk cache");
            let _ = output.send(WorkerEvent::CacheWarning {
                parent: job.parent.clone(),
                revision: job.revision,
                message: format!("Disk cache unavailable: {error} · sizes kept for this session"),
            });
        }
    }
    Ok(Arc::new(Snapshot::new(scan, job.volume.clone())))
}

fn volume_label(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return "Free unknown · unknown".into();
        };
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // statvfs reads the containing volume only; no doctor subprocesses or
        // user-directory probes are needed for the browser header.
        if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return "Free unknown · unknown".into();
        }
        let stats = unsafe { stats.assume_init() };
        #[allow(clippy::unnecessary_cast)]
        let free = (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64);
        #[cfg(target_os = "macos")]
        let kind = {
            let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
            if unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) } == 0 {
                let stats = unsafe { stats.assume_init() };
                unsafe { std::ffi::CStr::from_ptr(stats.f_fstypename.as_ptr()) }
                    .to_string_lossy()
                    .to_uppercase()
            } else {
                "unknown".to_owned()
            }
        };
        #[cfg(not(target_os = "macos"))]
        let kind = "unknown";
        format!("Free {} · {kind}", crate::human_bytes(free))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        "Free unknown · unknown".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn progressive_children_match_scanner_and_deduplicate_cross_child_hardlinks() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        for dir in ["alpha/nested", "beta", "project/node_modules"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("alpha/nested/payload"), vec![7; 8192]).unwrap();
        std::fs::write(root.join("beta/payload"), vec![8; 16384]).unwrap();
        std::fs::write(root.join("project/package.json"), b"{}").unwrap();
        std::fs::write(root.join("project/node_modules/cache"), vec![9; 4096]).unwrap();
        std::fs::write(root.join("top"), b"hello").unwrap();
        std::fs::hard_link(root.join("alpha/nested/payload"), root.join("beta/shared")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("alpha"), root.join("link")).unwrap();
        let reference = Scanner::new(ScanOptions::default()).scan(&root).unwrap();
        let workers = Workers::start();
        workers
            .requests
            .send(Request::Browse {
                path: root.clone(),
                revision: 0,
            })
            .unwrap();
        let mut completed = 0;
        let mut total = 0;
        let mut saw_list = false;
        let mut explanation_source = None;
        while completed < 3 {
            match workers
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                WorkerEvent::Listed {
                    rows,
                    total: listed_total,
                    ..
                } => {
                    saw_list = true;
                    assert_eq!(rows.iter().filter(|r| r.pending).count(), 3);
                    assert!(
                        rows.iter()
                            .any(|r| r.name == "top" && !r.pending && r.summary.allocated > 0)
                    );
                    #[cfg(unix)]
                    assert!(
                        rows.iter()
                            .any(|r| r.name == "link" && r.kind == NodeKind::Symlink && !r.pending)
                    );
                    total = listed_total;
                }
                WorkerEvent::ChildFinished {
                    path,
                    snapshot,
                    total: current,
                    ..
                } => {
                    assert!(saw_list);
                    completed += 1;
                    explanation_source = Some(Arc::clone(&snapshot));
                    total = current;
                    let child = Scanner::new(ScanOptions::default()).scan(&path).unwrap();
                    assert_eq!(
                        (
                            snapshot.summaries[0].apparent,
                            snapshot.summaries[0].allocated
                        ),
                        crate::subtree_sizes(&child.arena, child.arena.root())
                    );
                    if path.ends_with("project") {
                        assert_eq!(snapshot.summaries[0].targets, 1);
                        let rows = snapshot.rows(snapshot.scan.arena.root());
                        assert_eq!(
                            rows.iter()
                                .find(|r| r.name == "node_modules")
                                .unwrap()
                                .summary
                                .target_id
                                .as_deref(),
                            Some("node.modules")
                        );
                    }
                }
                WorkerEvent::Error { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        assert_eq!(
            total,
            crate::subtree_sizes(&reference.arena, reference.arena.root()).1
        );
        let snapshot = explanation_source.unwrap();
        let index = snapshot.scan.arena.root();
        let expected = estimate_subtree(&snapshot.scan.arena, index, true);
        workers
            .requests
            .send(Request::Explain {
                path: root,
                revision: 0,
                snapshot,
                index,
            })
            .unwrap();
        loop {
            match workers.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                WorkerEvent::Explained { estimate, .. } => {
                    assert_eq!(estimate, expected);
                    break;
                }
                WorkerEvent::Error { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        workers.requests.send(Request::Quit).unwrap();
    }

    #[test]
    fn cancellation_stops_queued_jobs_and_returns_promptly() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        for n in 0..40 {
            std::fs::create_dir_all(root.join(format!("child-{n}/a/b/c/d"))).unwrap();
        }
        let mut workers = Workers::start();
        workers
            .requests
            .send(Request::Browse {
                path: root.clone(),
                revision: 0,
            })
            .unwrap();
        loop {
            if matches!(
                workers.events.recv_timeout(Duration::from_secs(5)).unwrap(),
                WorkerEvent::Listed { .. }
            ) {
                break;
            }
        }
        let start = Instant::now();
        workers.requests.send(Request::Quit).unwrap();
        workers.handle.take().unwrap().join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
        let completed = workers
            .events
            .try_iter()
            .filter(|event| matches!(event, WorkerEvent::ChildFinished { .. }))
            .count();
        assert!(completed < 40, "queued children must be cancelled");
        let job = ScanJob {
            parent: root.clone(),
            path: root,
            revision: 0,
            volume: "unknown".into(),
            cancellation: Arc::new(AtomicBool::new(true)),
            cache: None,
            allow_cache: false,
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        assert!(scan_child(&job, &tx).unwrap_err().contains("cancelled"));
        assert!(rx.is_empty());
    }

    #[test]
    fn snapshot_deduplicates_each_cached_scope_and_propagates_coverage() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("nested/deeper")).unwrap();
        std::fs::write(root.join("nested/a"), vec![0; 8192]).unwrap();
        std::fs::hard_link(root.join("nested/a"), root.join("nested/deeper/b")).unwrap();
        let mut scan = Scanner::new(ScanOptions::default()).scan(&root).unwrap();
        scan.coverage_errors.push(cleanrs_core::fs::CoverageError {
            path: root.join("nested/deeper/vanished"),
            kind: "test".into(),
            message: "synthetic inaccessible child".into(),
        });
        let snapshot = Snapshot::new(scan, "unknown".into());
        for index in snapshot.directories.values() {
            let (apparent, allocated) = crate::subtree_sizes(&snapshot.scan.arena, *index);
            assert_eq!(snapshot.summaries[index.as_usize()].apparent, apparent);
            assert_eq!(snapshot.summaries[index.as_usize()].allocated, allocated);
            assert!(snapshot.summaries[index.as_usize()].incomplete);
        }
    }
    #[test]
    fn browser_scan_rebases_nested_arenas_preserves_hardlinks_and_falls_back() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("project/.next/deep")).unwrap();
        std::fs::create_dir(root.join("project/dist")).unwrap();
        std::fs::write(root.join("project/package.json"), b"{}").unwrap();
        std::fs::write(root.join("project/.next/deep/a"), vec![0; 8192]).unwrap();
        std::fs::hard_link(
            root.join("project/.next/deep/a"),
            root.join("project/dist/b"),
        )
        .unwrap();
        let snapshot = Arc::new(Snapshot::new(
            Scanner::new(Default::default()).scan(&root).unwrap(),
            "test".into(),
        ));
        let project = snapshot.directories[&root.join("project")];
        let sources = || {
            snapshot
                .scan
                .arena
                .children(project)
                .filter(|i| snapshot.scan.arena.nodes[i.as_usize()].kind == NodeKind::Directory)
                .map(|i| (snapshot.clone(), i))
                .collect()
        };
        let scan = BrowserScan { sources: sources() }
            .assemble(&root.join("project"))
            .unwrap()
            .unwrap();
        let reference = Scanner::new(Default::default())
            .scan(&root.join("project"))
            .unwrap();
        let definitions = BUILTIN_TARGETS.to_vec();
        let actual = cleanrs_core::targets::detect_workspace_targets(&scan, &definitions, None);
        let expected =
            cleanrs_core::targets::detect_workspace_targets(&reference, &definitions, None);
        assert_eq!(actual.len(), expected.len());
        for target in &actual {
            let other = expected.iter().find(|t| t.id == target.id).unwrap();
            assert_eq!(target.path, other.path);
            assert_eq!(target.usage, other.usage);
        }
        assert_eq!(
            crate::subtree_sizes(&scan.arena, scan.arena.root()),
            crate::subtree_sizes(&reference.arena, reference.arena.root())
        );
        struct NoScan;
        impl PlanObserver for NoScan {
            fn on_event(&self, _: PlanEvent) {}
            fn on_scan_progress(&self, _: ScanProgress) {
                panic!("cached discovery must not scan");
            }
        }
        let finished = snapshot.finished;
        let plan = pipeline::build_plan_from_scan(
            plan_request(
                root.join("project"),
                Some(root.join("home")),
                CleanupLevel::Safe,
                false,
                None,
            ),
            Arc::new(NoScan),
            scan,
            finished,
            Duration::from_secs(400 * 86400),
        )
        .unwrap();
        assert_eq!(plan.scan_finished, Some(finished));
        assert_eq!(plan.scan_age_offset, Duration::from_secs(400 * 86400));
        assert_eq!(plan.targets.len(), actual.len());
        std::fs::create_dir(root.join("project/new-directory")).unwrap();
        assert!(
            BrowserScan { sources: sources() }
                .assemble(&root.join("project"))
                .unwrap()
                .is_none()
        );
    }
}
