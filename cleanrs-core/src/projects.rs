//! Read-only project hygiene. Repository state is evidence, never permission to
//! delete source. Artifact eligibility remains the shared target planner's job.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::fs::{NodeIndex, NodeKind, ScanResult};
use crate::providers::ProviderHost;
use crate::targets::{CandidateSafety, CommandSpec, DetectedTarget, PlannedOperation};

/// Explicit, user-owned policy. No project-local configuration is loaded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionRule {
    pub path: PathBuf,
    #[serde(default)]
    pub project_only: bool,
    pub artifact_days: u32,
    #[serde(default = "default_dormant_days")]
    pub dormant_days: u32,
    #[serde(default)]
    pub keep: bool,
}
const fn default_dormant_days() -> u32 {
    90
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProjectInventory {
    pub scope: PathBuf,
    pub coverage_complete: bool,
    pub projects: Vec<ProjectReport>,
    pub diagnostics: Vec<String>,
    pub inspected_repositories: usize,
    pub inspection_commands: usize,
    pub omitted_repositories: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Synchronization {
    Synchronized,
    Unpushed,
    Behind,
    Diverged,
    NoUpstream,
    RemoteLess,
    Unknown,
}

/// Independent flags deliberately preserve combinations such as dirty+unpushed.
#[derive(Debug, Clone, Serialize)]
pub struct GitEvidence {
    pub dirty: bool,
    pub untracked: bool,
    pub has_remote: Option<bool>,
    pub synchronization: Synchronization,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub last_commit_unix_seconds: Option<i64>,
    pub worktrees: Vec<PathBuf>,
    pub outside_scope_worktrees: Vec<PathBuf>,
    /// Requested source-protection checks completed. A selection-only review
    /// deliberately leaves activity uninspected rather than assuming clean.
    pub complete: bool,
    pub activity_inspected: bool,
    pub filters_disabled: bool,
    pub inspection_error: Option<String>,
    /// Only artifact roots are retained; source filenames are not cached.
    pub tracked_artifact_roots: BTreeSet<PathBuf>,
}
impl Default for GitEvidence {
    fn default() -> Self {
        Self {
            dirty: false,
            untracked: false,
            has_remote: None,
            synchronization: Synchronization::Unknown,
            ahead: None,
            behind: None,
            last_commit_unix_seconds: None,
            worktrees: vec![],
            outside_scope_worktrees: vec![],
            complete: false,
            activity_inspected: false,
            filters_disabled: false,
            inspection_error: None,
            tracked_artifact_roots: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectActivity {
    Active,
    DormantCandidate,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactReport {
    pub target_id: String,
    pub path: PathBuf,
    pub ecosystem: String,
    pub storage_kind: String,
    pub recommended: bool,
    pub supporting_evidence: Vec<String>,
    pub weakening_evidence: Vec<String>,
    /// Includes the interval, confidence, exact action, and rebuild consequence.
    pub target: DetectedTarget,
}

/// Metadata evidence only: dependency lockfiles are not process locks.
#[derive(Debug, Clone, Serialize)]
pub struct ActivitySignal {
    pub kind: String,
    pub path: PathBuf,
    pub modified_unix_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectReport {
    pub root: PathBuf,
    /// Attributed storage, not permission or a proposed repository deletion.
    pub usage: crate::targets::UsageEstimate,
    pub git: Option<GitEvidence>,
    pub newest_source_unix_seconds: Option<i64>,
    pub newest_source_path: Option<PathBuf>,
    pub activity_signals: Vec<ActivitySignal>,
    pub recommendation: String,
    pub inactivity_days: Option<u64>,
    pub dormant_after_days: u32,
    pub activity: ProjectActivity,
    pub protected: bool,
    pub evidence: Vec<String>,
    pub artifacts: Vec<ArtifactReport>,
    /// References to shared directories reported once under their primary owner.
    pub shared_artifacts: Vec<PathBuf>,
}

/// Limits are deliberately global, not one worker per repository. The host
/// additionally bounds every command's runtime and captured output.
#[derive(Debug, Clone)]
pub struct InspectionLimits {
    /// Selection review checks admin metadata and tracked paths, without status
    /// reading source outside the selected metadata-only scan.
    pub review_only: bool,
    pub home: Option<PathBuf>,
    pub repositories: usize,
    pub commands: usize,
    pub elapsed: Duration,
}
impl Default for InspectionLimits {
    fn default() -> Self {
        Self {
            home: None,
            review_only: false,
            repositories: 128,
            commands: 768,
            elapsed: Duration::from_secs(15),
        }
    }
}

struct GitInspector<'a> {
    host: &'a dyn ProviderHost,
    executable: Option<PathBuf>,
    started: Instant,
    limits: InspectionLimits,
    commands: usize,
    config_overrides: Vec<String>,
}
impl GitInspector<'_> {
    fn run(&mut self, root: &Path, args: &[&str]) -> Option<String> {
        if self.commands >= self.limits.commands || self.started.elapsed() >= self.limits.elapsed {
            return None;
        }
        let executable = self.executable.clone()?;
        self.commands += 1;
        // Disabling fsmonitor prevents repository configuration from launching
        // a hook during status. Optional locks and lazy object fetching are off.
        let mut argv: Vec<String> = [
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "core.excludesFile=/dev/null",
            "-C",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        argv.push(root.to_string_lossy().into_owned());
        argv.extend(self.config_overrides.iter().cloned());
        argv.extend(args.iter().map(|s| (*s).to_owned()));
        let mut command = CommandSpec {
            executable,
            args: argv,
            environment: BTreeMap::from([
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                ("GIT_NO_LAZY_FETCH".into(), "1".into()),
                ("GIT_LITERAL_PATHSPECS".into(), "1".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("LC_ALL".into(), "C".into()),
            ]),
        };
        // check-ignore does not accept Git pathspec magic (including the
        // literal environment flag); its operands are exact filesystem names.
        if args.first() == Some(&"check-ignore") {
            command.environment.remove("GIT_LITERAL_PATHSPECS");
        }
        let output = self.host.run_inspection(&command).ok()?;
        (output.exit_code == Some(0)).then_some(output.stdout)
    }

    fn prepare(&mut self, root: &Path) -> bool {
        self.config_overrides.clear();
        // Listing config does not invoke filters. Explicitly disable includes
        // for this query, then refuse them before running status/log at all.
        let Some(config) = self.run(root, &["config", "--null", "--list", "--no-includes"]) else {
            return false;
        };
        let mut drivers = BTreeSet::new();
        for record in config.split('\0') {
            let key = record.split('\n').next().unwrap_or("");
            let lower = key.to_ascii_lowercase();
            if lower.starts_with("include.")
                || lower.starts_with("includeif.")
                || lower == "extensions.partialclone"
                || lower.starts_with("remote.") && lower.ends_with(".promisor")
            {
                return false;
            }
            if let Some(driver) = key
                .strip_prefix("filter.")
                .and_then(|s| s.rsplit_once('.').map(|(driver, _)| driver))
            {
                drivers.insert(driver.to_owned());
            }
        }
        for driver in drivers {
            for (key, value) in [
                ("clean", ""),
                ("smudge", ""),
                ("process", ""),
                ("required", "false"),
            ] {
                self.config_overrides
                    .extend(["-c".into(), format!("filter.{driver}.{key}={value}")]);
            }
        }
        true
    }

    fn tracked_artifacts(
        &mut self,
        root: &Path,
        artifacts: &[PathBuf],
    ) -> Option<BTreeSet<PathBuf>> {
        let relative: Vec<_> = artifacts
            .iter()
            .filter_map(|p| p.strip_prefix(root).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if relative.is_empty() {
            return Some(BTreeSet::new());
        }
        let mut args = vec!["ls-files", "-z", "--cached", "--"];
        args.extend(relative.iter().map(String::as_str));
        let output = self.run(root, &args)?;
        let mut vetoed = BTreeSet::new();
        for tracked in output
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(|p| root.join(p))
        {
            vetoed.extend(
                artifacts
                    .iter()
                    .filter(|artifact| tracked.starts_with(artifact))
                    .cloned(),
            );
        }
        Some(vetoed)
    }

    fn inspect_for_review(&mut self, root: &Path, artifacts: &[PathBuf]) -> GitEvidence {
        let mut evidence = GitEvidence::default();
        if self.prepare(root) {
            if let Some(roots) = self.tracked_artifacts(root, artifacts) {
                evidence.tracked_artifact_roots = roots;
                evidence.complete = true;
            }
        }
        evidence
    }

    fn inspect(&mut self, root: &Path, scope: &Path, artifacts: &[PathBuf]) -> GitEvidence {
        let mut evidence = GitEvidence::default();
        if !self.prepare(root) {
            evidence.inspection_error = Some("Git config inspection unavailable or external includes/partial-clone metadata refused".into());
            return evidence;
        }
        evidence.filters_disabled = !self.config_overrides.is_empty();
        evidence.activity_inspected = true;
        let status = self.run(
            root,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=normal",
                "--ignore-submodules=all",
            ],
        );
        if let Some(status) = status.as_deref() {
            parse_status(status, &mut evidence);
        }
        let remotes = self.run(root, &["remote"]);
        evidence.has_remote = remotes.as_ref().map(|s| !s.trim().is_empty());
        if evidence.has_remote == Some(false) {
            evidence.synchronization = Synchronization::RemoteLess;
        }
        let commit = self.run(root, &["log", "-1", "--format=%ct"]);
        evidence.last_commit_unix_seconds = commit.as_ref().and_then(|s| s.trim().parse().ok());
        let worktrees = self.run(root, &["worktree", "list", "--porcelain", "-z"]);
        if let Some(output) = &worktrees {
            for record in output.split('\0') {
                if let Some(path) = record.strip_prefix("worktree ") {
                    let path = PathBuf::from(path);
                    if !path.starts_with(scope) {
                        evidence.outside_scope_worktrees.push(path.clone());
                    }
                    evidence.worktrees.push(path);
                }
            }
        }
        // Query only artifact paths; retain vetoed roots rather than filenames.
        let tracked = self.tracked_artifacts(root, artifacts);
        if let Some(roots) = &tracked {
            evidence.tracked_artifact_roots = roots.clone();
        }
        evidence.complete =
            status.is_some() && remotes.is_some() && worktrees.is_some() && tracked.is_some();
        if !evidence.complete {
            evidence.inspection_error = Some("Git inspection failed, exceeded output/time bounds, or exhausted its command budget".into());
        }
        // An unborn HEAD is observable but cannot establish repository dormancy.
        evidence
    }
}

fn parse_status(status: &str, evidence: &mut GitEvidence) {
    let mut upstream = false;
    let mut skip_original_path = false;
    for record in status.split('\0') {
        if skip_original_path {
            skip_original_path = false;
            continue;
        }
        if record.starts_with("# branch.upstream ") {
            upstream = true;
        }
        if let Some(counts) = record.strip_prefix("# branch.ab ") {
            let mut counts = counts.split_whitespace();
            evidence.ahead = counts
                .next()
                .and_then(|s| s.strip_prefix('+'))
                .and_then(|s| s.parse().ok());
            evidence.behind = counts
                .next()
                .and_then(|s| s.strip_prefix('-'))
                .and_then(|s| s.parse().ok());
        }
        evidence.untracked |= record.starts_with("? ");
        evidence.dirty |=
            record.starts_with("1 ") || record.starts_with("2 ") || record.starts_with("u ");
        skip_original_path = record.starts_with("2 ");
    }
    evidence.synchronization = match (upstream, evidence.ahead, evidence.behind) {
        (false, _, _) => Synchronization::NoUpstream,
        (true, Some(0), Some(0)) => Synchronization::Synchronized,
        (true, Some(0), Some(_)) => Synchronization::Behind,
        (true, Some(_), Some(0)) => Synchronization::Unpushed,
        (true, Some(_), Some(_)) => Synchronization::Diverged,
        _ => Synchronization::Unknown,
    };
}

/// Most-specific project rule wins over root rules. All paths are resolved by
/// the trusted registry loader before reaching the core.
pub fn retention_for<'a>(root: &Path, rules: &'a [RetentionRule]) -> Option<&'a RetentionRule> {
    rules
        .iter()
        .filter(|rule| {
            if rule.project_only {
                rule.path == root
            } else {
                root.starts_with(&rule.path)
            }
        })
        .max_by_key(|rule| (rule.project_only, rule.path.components().count()))
}

fn age_days(timestamp: i64, now: i64) -> Option<u64> {
    (timestamp > 0).then(|| now.saturating_sub(timestamp).max(0) as u64 / 86_400)
}

fn ignored_component(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(
                ".git"
                    | "node_modules"
                    | ".venv"
                    | "venv"
                    | "target"
                    | ".next"
                    | "dist"
                    | ".gradle"
            )
        )
    })
}

/// Enrich the existing candidates in place so CLI/TUI/manual selection all use
/// identical retention and tracked-source refusals. The report itself is inert.
pub fn inspect_projects(
    scan: &ScanResult,
    targets: &mut [DetectedTarget],
    rules: &[RetentionRule],
    host: &dyn ProviderHost,
    limits: InspectionLimits,
) -> ProjectInventory {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    inspect_projects_at(scan, targets, rules, host, limits, now)
}

pub fn inspect_projects_at(
    scan: &ScanResult,
    targets: &mut [DetectedTarget],
    rules: &[RetentionRule],
    host: &dyn ProviderHost,
    limits: InspectionLimits,
    now: i64,
) -> ProjectInventory {
    let mut inventory = ProjectInventory {
        scope: scan.root.clone(),
        coverage_complete: scan.coverage_complete(),
        ..Default::default()
    };
    let mut roots = BTreeSet::new();
    let mut repositories = BTreeMap::new();
    let mut newest = BTreeMap::<PathBuf, i64>::new();
    let mut newest_paths = BTreeMap::<PathBuf, PathBuf>::new();
    let mut signals = BTreeMap::<PathBuf, Vec<ActivitySignal>>::new();
    let mut nodes = BTreeMap::new();
    // One metadata pass discovers projects without reading source files. Git
    // metadata must be entirely inside the safe scan before invoking Git.
    for index in (0..scan.arena.nodes.len()).map(NodeIndex::from_usize) {
        let path = scan.arena.path(index);
        if scan.arena.nodes[index.as_usize()].kind == NodeKind::Directory {
            nodes.insert(path.clone(), index);
        }
        let relative = path.strip_prefix(&scan.root).unwrap_or(&path);
        let name = scan.arena.name(index);
        if name == ".git" && !ignored_component(relative.parent().unwrap_or(Path::new(""))) {
            let root = path.parent().unwrap().to_path_buf();
            roots.insert(root.clone());
            let safe = crate::targets::validate_scanned_candidate(
                &scan.arena,
                index,
                limits.home.as_deref(),
            ) == CandidateSafety::Eligible;
            let project_index = scan.arena.parent(index).unwrap_or(scan.arena.root());
            let safe = safe
                && (!limits.review_only
                    || safe_admin_directory(&path, &scan.root, limits.home.as_deref())
                        .unwrap_or(false))
                && crate::targets::validate_scanned_candidate(
                    &scan.arena,
                    project_index,
                    limits.home.as_deref(),
                ) == CandidateSafety::Eligible
                && scan.coverage_complete()
                && git_metadata_in_scope(index, scan, limits.home.as_deref());
            repositories.insert(root, safe);
        }
        if !ignored_component(relative)
            && matches!(
                name,
                "Cargo.toml"
                    | "package.json"
                    | "pyproject.toml"
                    | "requirements.txt"
                    | "build.gradle"
                    | "build.gradle.kts"
            )
        {
            roots.insert(path.parent().unwrap().to_path_buf());
        }
    }
    // A search begun inside a repository cannot claim to have inspected admin
    // metadata outside its explicit scope. Keep that containing repo unknown.
    let mut ancestor = scan.root.parent();
    while let Some(root) = ancestor {
        if std::fs::symlink_metadata(root.join(".git")).is_ok() {
            roots.insert(root.to_path_buf());
            repositories.insert(root.to_path_buf(), false);
        }
        ancestor = root.parent();
    }
    for target in targets.iter() {
        if let Some(ownership) = &target.cargo_ownership {
            roots.extend(ownership.observed_owners.iter().cloned());
        }
        if let Some(root) = &target.project_root {
            roots.insert(root.clone());
        }
    }
    for target in targets.iter_mut() {
        if let Some(path) = &target.path {
            if let Some(index) = nodes.get(path) {
                target.age_days =
                    crate::targets::modified_age_days(&scan.arena, *index, now.max(0) as u64);
                if target.id == "cargo.target" {
                    if let Some(manifest) = target.project_root.as_ref().and_then(|root| {
                        (0..scan.arena.nodes.len())
                            .map(NodeIndex::from_usize)
                            .find(|i| scan.arena.path(*i) == root.join("Cargo.toml"))
                    }) {
                        target.age_days = crate::targets::cargo_activity_age_days(
                            &scan.arena,
                            *index,
                            manifest,
                            now.max(0) as u64,
                        );
                    }
                    if target.cargo_ownership.is_none() {
                        target.safety = CandidateSafety::Refused("Cargo metadata ownership unavailable; marker alone cannot authorize build cleanup".into());
                    }
                }
                target.stale = target
                    .age_days
                    .map(|age| age >= u64::from(target.stale_after_days));
            }
        }
    }
    // Assign each source entry to its nearest project; propagate to ancestors
    // so an active nested package also weakens a dormant workspace conclusion.
    let artifact_paths: Vec<_> = targets.iter().filter_map(|t| t.path.as_ref()).collect();
    for index in (0..scan.arena.nodes.len()).map(NodeIndex::from_usize) {
        let node = &scan.arena.nodes[index.as_usize()];
        let path = scan.arena.path(index);
        let relative = path.strip_prefix(&scan.root).unwrap_or(&path);
        if node.kind != NodeKind::File
            || ignored_component(relative)
            || artifact_paths.iter().any(|p| path.starts_with(p))
        {
            continue;
        }
        let mut parent = path.parent();
        while let Some(p) = parent {
            if roots.contains(p) {
                if newest
                    .get(p)
                    .is_none_or(|time| node.modified_unix_seconds > *time)
                {
                    newest_paths.insert(p.to_path_buf(), path.clone());
                }
                if path.parent() == Some(p)
                    && matches!(
                        scan.arena.name(index),
                        "Cargo.lock"
                            | "package-lock.json"
                            | "pnpm-lock.yaml"
                            | "yarn.lock"
                            | "bun.lock"
                            | "bun.lockb"
                            | "uv.lock"
                            | "poetry.lock"
                            | "gradle.lockfile"
                            | "Cargo.toml"
                            | "package.json"
                            | "pyproject.toml"
                            | "build.gradle"
                            | "build.gradle.kts"
                    )
                {
                    signals
                        .entry(p.to_path_buf())
                        .or_default()
                        .push(ActivitySignal {
                            kind: "manifest_or_dependency_lockfile".into(),
                            path: path.clone(),
                            modified_unix_seconds: node.modified_unix_seconds,
                        });
                }
                newest
                    .entry(p.to_path_buf())
                    .and_modify(|n| *n = (*n).max(node.modified_unix_seconds))
                    .or_insert(node.modified_unix_seconds);
            }
            if p == scan.root {
                break;
            }
            parent = p.parent();
        }
    }
    let mut inspector = GitInspector {
        host,
        executable: host.resolve_executable("git").ok().flatten(),
        started: Instant::now(),
        limits,
        commands: 0,
        config_overrides: vec![],
    };
    for root in roots {
        let mut evidence = vec![
            "Repository/worktree deletion is never inferred from age or synchronization".into(),
        ];
        let git = if let Some(safe) = repositories.get(&root) {
            if !safe
                || inventory.inspected_repositories >= inspector.limits.repositories
                || inspector.started.elapsed() >= inspector.limits.elapsed
                || inspector.commands >= inspector.limits.commands
            {
                inventory.omitted_repositories += 1;
                evidence.push("Git inspection unavailable: unsafe metadata, scope, or inspection budget; state is unknown".into());
                Some(GitEvidence::default())
            } else {
                inventory.inspected_repositories += 1;
                let paths = targets
                    .iter()
                    .filter_map(|t| t.path.as_ref())
                    .filter(|p| p.starts_with(&root))
                    .cloned()
                    .collect::<Vec<_>>();
                Some(if inspector.limits.review_only {
                    inspector.inspect_for_review(&root, &paths)
                } else {
                    inspector.inspect(&root, &scan.root, &paths)
                })
            }
        } else {
            None
        };
        let rule = retention_for(&root, rules);
        let threshold = rule.map_or(90, |r| r.dormant_days);
        let protected = rule.is_some_and(|r| r.keep);
        let source = newest.get(&root).copied();
        let mut activity_time = source;
        if let Some(git) = &git {
            activity_time = activity_time
                .into_iter()
                .chain(git.last_commit_unix_seconds)
                .max();
            if git.filters_disabled {
                evidence.push("Git content filters were disabled; dirtiness may be conservatively overreported".into());
            }
            if let Some(error) = &git.inspection_error {
                evidence.push(error.clone());
            }
            if !git.activity_inspected {
                evidence.push("Repository activity is unknown; selection review checks tracked-source protection without reading unselected source".into());
            }
            evidence.push(format!(
                "Git: {:?}; dirty={}; untracked={}; complete={}",
                git.synchronization, git.dirty, git.untracked, git.complete
            ));
            if !git.outside_scope_worktrees.is_empty() {
                evidence.push(
                    "Related worktrees exist outside this search root; their activity is unknown"
                        .into(),
                );
            }
        }
        // Fresh build output is evidence of use even if source has not changed.
        let artifact_age = targets
            .iter()
            .filter(|t| t.project_root.as_ref() == Some(&root))
            .filter_map(|t| t.age_days)
            .min();
        let inactivity = activity_time
            .and_then(|t| age_days(t, now))
            .map(|age| artifact_age.map_or(age, |build| age.min(build)));
        let unknown_artifact_activity = targets
            .iter()
            .any(|t| t.project_root.as_ref() == Some(&root) && t.age_days.is_none());
        let uncertain_git = unknown_artifact_activity
            || git.as_ref().is_none_or(|g| {
                !g.complete
                    || g.last_commit_unix_seconds.is_none()
                    || !g.outside_scope_worktrees.is_empty()
            });
        let activity = if protected
            || git.as_ref().is_some_and(|g| g.dirty || g.untracked)
            || inactivity.is_some_and(|age| age < u64::from(threshold))
        {
            ProjectActivity::Active
        } else if !uncertain_git && scan.coverage_complete() && inactivity.is_some() {
            ProjectActivity::DormantCandidate
        } else {
            ProjectActivity::Unknown
        };
        evidence.push(format!(
            "Dormancy threshold: {threshold}d; artifact age alone never establishes abandonment"
        ));
        if protected {
            evidence.push(
                "Retention policy keeps this project's artifacts; manual naming is required".into(),
            );
        }
        let usage = nodes.get(&root).map_or_else(
            || crate::targets::UsageEstimate::Unknown {
                explanation: "project is outside the explicit scan".into(),
            },
            |index| {
                let (apparent, allocated) = crate::targets::subtree_sizes(&scan.arena, *index);
                crate::targets::UsageEstimate::Filesystem {
                    apparent,
                    allocated,
                    reclaimable: crate::accounting::estimate_subtree(
                        &scan.arena,
                        *index,
                        scan.coverage_complete(),
                    ),
                }
            },
        );
        let newest_source_path = newest_paths.remove(&root);
        if let Some(path) = &newest_source_path {
            evidence.push(format!(
                "Newest source/manifest activity: {} at {:?}",
                path.display(),
                source
            ));
        }
        let mut activity_signals = signals.remove(&root).unwrap_or_default();
        activity_signals.sort_by(|a, b| a.path.cmp(&b.path));
        inventory.projects.push(ProjectReport {
            root,
            newest_source_path,
            activity_signals,
            recommendation: String::new(),
            usage,
            git,
            newest_source_unix_seconds: source,
            inactivity_days: inactivity,
            dormant_after_days: threshold,
            activity,
            protected,
            evidence,
            artifacts: vec![],
            shared_artifacts: vec![],
        });
    }
    // Nested packages inherit repository evidence. Worktrees are independent
    // sources of activity: one active sibling weakens dormancy for the family.
    let repository_reports: Vec<_> = inventory
        .projects
        .iter()
        .filter(|p| p.git.is_some())
        .cloned()
        .collect();
    for project in &mut inventory.projects {
        if project.git.is_none() {
            if let Some(parent) = repository_reports
                .iter()
                .filter(|p| project.root.starts_with(&p.root))
                .max_by_key(|p| p.root.components().count())
            {
                project.git = parent.git.clone();
                project.evidence.push(format!(
                    "Repository evidence inherited from {}",
                    parent.root.display()
                ));
            }
        }
        if let Some(git) = &project.git {
            let related: Vec<_> = repository_reports
                .iter()
                .filter(|p| git.worktrees.contains(&p.root))
                .collect();
            let commit_age = git.last_commit_unix_seconds.and_then(|t| age_days(t, now));
            let related_age = related.iter().filter_map(|p| p.inactivity_days).min();
            project.inactivity_days = project
                .inactivity_days
                .into_iter()
                .chain(commit_age)
                .chain(related_age)
                .min();
            let dirty = git.dirty
                || git.untracked
                || related
                    .iter()
                    .any(|p| p.git.as_ref().is_some_and(|g| g.dirty || g.untracked));
            if related.iter().any(|p| {
                p.root != project.root
                    && (p.git.as_ref().is_some_and(|g| g.dirty || g.untracked)
                        || p.inactivity_days
                            .is_some_and(|days| days < u64::from(project.dormant_after_days)))
            }) {
                project.evidence.push("Related worktree has recent activity or local work; project dormancy is weakened".into());
            }
            let artifact_activity_known = targets
                .iter()
                .filter(|target| target.project_root.as_ref() == Some(&project.root))
                .all(|target| target.age_days.is_some());
            let all_known = artifact_activity_known
                && git.complete
                && git.activity_inspected
                && git.last_commit_unix_seconds.is_some()
                && git.outside_scope_worktrees.is_empty()
                && git.worktrees.iter().all(|root| {
                    repository_reports.iter().any(|p| {
                        &p.root == root
                            && p.inactivity_days.is_some()
                            && p.git.as_ref().is_some_and(|g| g.complete)
                    })
                });
            project.activity = if project.protected
                || dirty
                || project
                    .inactivity_days
                    .is_some_and(|age| age < u64::from(project.dormant_after_days))
            {
                ProjectActivity::Active
            } else if all_known && scan.coverage_complete() && project.inactivity_days.is_some() {
                ProjectActivity::DormantCandidate
            } else {
                ProjectActivity::Unknown
            };
        }
    }
    for project in &mut inventory.projects {
        project.recommendation = match project.activity {
            ProjectActivity::DormantCandidate => "Review inactive project/worktree and its generated artifacts; repository deletion remains explicit-only and has no automatic operation",
            ProjectActivity::Active => "Keep project source; review independently stale generated artifacts",
            ProjectActivity::Unknown => "Project dormancy is unproven; review artifact evidence individually",
        }.into();
    }
    // Apply safety from every containing repository, including an outer repo
    // around a nested package. Missing Git evidence cannot authorize deletion.
    for target in targets.iter_mut() {
        let Some(path) = &target.path else {
            continue;
        };
        for project in &inventory.projects {
            if !path.starts_with(&project.root) {
                continue;
            }
            if let Some(git) = &project.git {
                if !git.complete
                    || git
                        .tracked_artifact_roots
                        .iter()
                        .any(|tracked| tracked.starts_with(path))
                {
                    target.safety = CandidateSafety::Refused("repository inspection incomplete or artifact contains tracked source; cleanup refused".into());
                }
            }
        }
        let Some(root) = &target.project_root else {
            continue;
        };
        if let Some(ownership) = &target.cargo_ownership {
            target.evidence.push(format!(
                "Cargo owners: {:?}; complete={}",
                ownership.observed_owners, ownership.complete
            ));
            if !ownership.complete {
                target.safety = CandidateSafety::Refused("shared/custom Cargo ownership is incomplete; no single project can authorize cleanup".into());
            }
        }
        if let Some(rule) = retention_for(root, rules) {
            if rule.keep {
                target.properties.automation = crate::targets::Automation::ExplicitTargetOnly;
            }
            target.stale_after_days = rule.artifact_days;
            target.stale = target
                .age_days
                .map(|age| !rule.keep && age >= u64::from(rule.artifact_days));
            target.evidence.push(format!(
                "Retention policy {}: {}d{}",
                rule.path.display(),
                rule.artifact_days,
                if rule.keep {
                    "; keep unless manually named"
                } else {
                    ""
                }
            ));
        }
        // A shared directory obeys every observed owner's retention policy.
        if let Some(ownership) = &target.cargo_ownership {
            for owner in &ownership.observed_owners {
                if let Some(rule) = retention_for(owner, rules) {
                    if rule.keep {
                        target.properties.automation =
                            crate::targets::Automation::ExplicitTargetOnly;
                    }
                    target.stale_after_days = target.stale_after_days.max(rule.artifact_days);
                    target.stale = target.age_days.map(|age| {
                        !rule.keep
                            && target.stale == Some(true)
                            && age >= u64::from(target.stale_after_days)
                    });
                }
            }
        }
        let Some(project) = inventory.projects.iter_mut().find(|p| &p.root == root) else {
            continue;
        };
        let mut supporting = vec![];
        let mut weakening = vec![];
        if target.stale == Some(true) {
            supporting.push(format!(
                "artifact inactive for {:?} days (threshold {}d)",
                target.age_days, target.stale_after_days
            ));
        }
        if project.activity == ProjectActivity::DormantCandidate {
            supporting
                .push("repository and filesystem evidence agree with project inactivity".into());
        } else {
            weakening.push(format!(
                "project activity: {:?}; stale artifacts do not imply dormant source",
                project.activity
            ));
        }
        if let Some(git) = &project.git {
            if git.dirty
                || git.untracked
                || !matches!(git.synchronization, Synchronization::Synchronized)
            {
                weakening.push("unsynced or local work protects the repository; independently generated artifacts can still be cleaned".into());
            }
        }
        if !scan.coverage_complete() {
            target.safety =
                CandidateSafety::Refused("project artifact scan has incomplete coverage".into());
        }
        if !matches!(target.safety, CandidateSafety::Eligible) {
            weakening.push(format!("safety: {:?}", target.safety));
        }
        if matches!(target.operation, PlannedOperation::Informational) || target.explicit_only() {
            weakening.push("environment/user data requires individual review; never selected by a cleanup level".into());
        }
        let recommended = target.stale == Some(true)
            && target.manually_selectable()
            && !target.explicit_only()
            && !project.protected;
        target
            .evidence
            .extend(supporting.iter().chain(&weakening).cloned());
        project.artifacts.push(ArtifactReport {
            target_id: target.id.clone(),
            path: path.clone(),
            ecosystem: target.id.split('.').next().unwrap_or("custom").into(),
            storage_kind: match target.id.as_str() {
                "node.modules" => "project_dependency_copy",
                "python.venv" | "python.requirements-venv" => "project_environment",
                "gradle.cache" | "gradle.kotlin-cache" => "project_local_cache",
                "cargo.target"
                | "node.next"
                | "node.dist"
                | "gradle.build"
                | "gradle.kotlin-build"
                | "xcode.project-build" => "project_build_output",
                _ => "project_local_artifact",
            }
            .into(),
            recommended,
            supporting_evidence: supporting,
            weakening_evidence: weakening,
            target: target.clone(),
        });
    }
    for target in targets.iter() {
        if let Some(ownership) = &target.cargo_ownership {
            for owner in &ownership.observed_owners {
                if target.project_root.as_ref() == Some(owner) {
                    continue;
                }
                if let Some(project) = inventory.projects.iter_mut().find(|p| &p.root == owner) {
                    project
                        .shared_artifacts
                        .push(ownership.target_directory.clone());
                    project.evidence.push(format!("Shared Cargo output {} is reported once under {:?}; this owner cannot authorize cleanup alone", ownership.target_directory.display(), target.project_root));
                }
            }
        }
    }
    for project in &mut inventory.projects {
        project
            .artifacts
            .sort_by_key(|a| std::cmp::Reverse(a.target.usage.sortable_bytes()));
    }
    inventory.projects.sort_by_key(|p| {
        (
            p.activity != ProjectActivity::DormantCandidate,
            std::cmp::Reverse(
                p.artifacts
                    .iter()
                    .filter(|a| a.recommended)
                    .map(|a| a.target.usage.sortable_bytes())
                    .max()
                    .unwrap_or_else(|| p.usage.sortable_bytes()),
            ),
            std::cmp::Reverse(p.inactivity_days.unwrap_or(0)),
            p.root.clone(),
        )
    });
    inventory.inspection_commands = inspector.commands;
    if inventory.omitted_repositories > 0 {
        inventory.diagnostics.push(format!(
            "{} repositories omitted; unknown state is not clean state",
            inventory.omitted_repositories
        ));
    }
    inventory.diagnostics.push("Git synchronization reflects local refs only; inspection never fetches. No live-process liveness is inferred from lockfile age.".into());
    inventory
}

// Reject alternate object stores: they can make an otherwise local Git
// command read cloud or out-of-scope metadata without any network request.
fn common_admin_directory(directory: &Path, scope: &Path) -> Option<PathBuf> {
    let common = directory.join("commondir");
    let common = if common.exists() {
        let metadata = std::fs::symlink_metadata(&common).ok()?;
        if !metadata.is_file() || metadata.len() > 4096 {
            return None;
        }
        let value = std::fs::read_to_string(common).ok()?;
        directory.join(value.trim()).canonicalize().ok()?
    } else {
        directory.to_path_buf()
    };
    if !common.starts_with(scope) {
        return None;
    }
    for name in ["objects/info/alternates", "objects/info/http-alternates"] {
        if std::fs::symlink_metadata(common.join(name)).is_ok() {
            return None;
        }
    }
    Some(common)
}

fn safe_admin_subtree(scan: &ScanResult, index: NodeIndex, home: Option<&Path>) -> bool {
    let mut pending = vec![index];
    while let Some(index) = pending.pop() {
        if scan.arena.nodes[index.as_usize()].kind == NodeKind::Symlink {
            return false;
        }
        pending.extend(scan.arena.children(index));
    }
    crate::targets::validate_scanned_candidate(&scan.arena, index, home)
        == CandidateSafety::Eligible
}

fn git_metadata_in_scope(index: NodeIndex, scan: &ScanResult, home: Option<&Path>) -> bool {
    let path = scan.arena.path(index);
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return false;
    };
    let directory = if metadata.is_dir() {
        if !safe_admin_subtree(scan, index, home) {
            return false;
        }
        path
    } else {
        if !metadata.is_file() || metadata.len() > 4096 {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Some(gitdir) = text.trim().strip_prefix("gitdir: ") else {
            return false;
        };
        let Ok(resolved) = path.parent().unwrap().join(gitdir).canonicalize() else {
            return false;
        };
        let Some(index) = (0..scan.arena.nodes.len())
            .map(NodeIndex::from_usize)
            .find(|i| scan.arena.path(*i) == resolved)
        else {
            return false;
        };
        if !safe_admin_subtree(scan, index, home) {
            return false;
        }
        resolved
    };
    let Some(common) = common_admin_directory(&directory, &scan.root) else {
        return false;
    };
    if common == directory {
        return true;
    }
    (0..scan.arena.nodes.len())
        .map(NodeIndex::from_usize)
        .find(|i| scan.arena.path(*i) == common)
        .is_some_and(|i| safe_admin_subtree(scan, i, home))
}

/// Git's ignore probe may read ancestor .gitignore files. Check those exact
/// paths before spawning it, including during a shallow selection review.
fn safe_ignore_files(root: &Path, path: &Path) -> bool {
    use crate::fs::{FileSystem, NativeFileSystem, NodeFlags};
    let mut cursor = path.parent();
    while let Some(directory) = cursor.filter(|directory| directory.starts_with(root)) {
        let ignore = directory.join(".gitignore");
        match NativeFileSystem.metadata(&ignore) {
            Ok(metadata)
                if metadata.kind == NodeKind::File
                    && !metadata.flags.contains(NodeFlags::DATALESS) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return false,
        }
        cursor = directory.parent();
    }
    true
}

/// Ambiguous names are recognized only with repository-local ignore evidence.
/// No Git source contents or cloud placeholders are read by this probe.
pub fn gitignore_evidence(
    scan: &ScanResult,
    paths: &[PathBuf],
    home: Option<&Path>,
) -> BTreeMap<PathBuf, crate::targets::GitIgnoreEvidence> {
    let mut evidence = BTreeMap::new();
    if !scan.coverage_complete() || paths.is_empty() {
        return evidence;
    }
    let mut roots = (0..scan.arena.nodes.len())
        .map(NodeIndex::from_usize)
        .filter(|i| scan.arena.name(*i) == ".git")
        .filter_map(|i| {
            scan.arena
                .parent(i)
                .map(|parent| (scan.arena.path(parent), i, parent))
        })
        .collect::<Vec<_>>();
    roots.sort_by_key(|(root, _, _)| std::cmp::Reverse(root.components().count()));
    let host = crate::providers::SystemProviderHost::project_inspection();
    // A single global command/time budget covers all ambiguous names. A large
    // checkout must not reset the subprocess budget once per build directory.
    let mut inspector = GitInspector {
        host: &host,
        executable: host.resolve_executable("git").ok().flatten(),
        started: Instant::now(),
        limits: InspectionLimits::default(),
        commands: 0,
        config_overrides: vec![],
    };
    let mut assigned = BTreeSet::new();
    for (root, admin, parent) in roots.into_iter().take(inspector.limits.repositories) {
        let candidates: Vec<_> = paths
            .iter()
            .filter(|path| path.starts_with(&root) && assigned.insert((*path).clone()))
            .collect();
        if candidates.is_empty()
            || crate::targets::validate_scanned_candidate(&scan.arena, parent, home)
                != CandidateSafety::Eligible
            || !(if scan.backend == "selected review" {
                safe_admin_directory(&root.join(".git"), &scan.root, home) == Ok(true)
            } else {
                git_metadata_in_scope(admin, scan, home)
            })
            || !inspector.prepare(&root)
        {
            continue;
        }
        for path in candidates {
            if !safe_ignore_files(&root, path) {
                continue;
            }
            let Ok(relative) = path.strip_prefix(&root) else {
                continue;
            };
            let Some(relative_text) = relative.to_str() else {
                continue;
            };
            if inspector
                .run(&root, &["check-ignore", "--quiet", "--", relative_text])
                .is_none()
            {
                continue;
            }
            if let Some(pattern) =
                inspector.run(&root, &["check-ignore", "--verbose", "--", relative_text])
            {
                evidence.insert(
                    path.clone(),
                    crate::targets::GitIgnoreEvidence {
                        repository: root.clone(),
                        relative_path: relative.to_path_buf(),
                        pattern: pattern.trim().to_owned(),
                    },
                );
            }
        }
    }
    evidence
}

/// Re-check source protection immediately before an artifact action. Dirty and
/// unpushed work do not veto generated output; tracked files always do.
pub fn revalidate_artifact_repository(
    target: &DetectedTarget,
    scope: &Path,
    home: Option<&Path>,
    host: &dyn ProviderHost,
) -> Result<(), String> {
    let Some(path) = &target.path else {
        return Ok(());
    };
    if target.project_root.is_none() {
        return Ok(());
    }
    let mut ignore_revalidated = target.gitignore.is_none();
    let mut cursor = path.parent();
    while let Some(root) = cursor {
        let admin = root.join(".git");
        if std::fs::symlink_metadata(&admin).is_ok() {
            if !root.starts_with(scope) {
                return Err("containing repository metadata is outside the explicit scope".into());
            }
            let safe = safe_admin_directory(&admin, scope, home)?;
            if !safe {
                return Err("repository metadata is unsafe or incomplete".into());
            }
            let mut inspector = GitInspector {
                host,
                executable: host.resolve_executable("git").map_err(|e| e.to_string())?,
                started: Instant::now(),
                limits: InspectionLimits::default(),
                commands: 0,
                config_overrides: vec![],
            };
            if !inspector.prepare(root) {
                return Err("repository configuration is unsafe or unavailable".into());
            }
            if let Some(ignore) = &target.gitignore {
                if ignore.repository == root {
                    if !safe_ignore_files(root, path) {
                        return Err(
                            "ignore metadata became symlinked, dataless, or unreadable".into()
                        );
                    }
                    ignore_revalidated = true;
                }
                if ignore.repository == root
                    && inspector
                        .run(
                            root,
                            &[
                                "check-ignore",
                                "--quiet",
                                "--",
                                &ignore.relative_path.to_string_lossy(),
                            ],
                        )
                        .is_none()
                {
                    return Err("required gitignore evidence changed after review".into());
                }
            }
            let tracked = inspector
                .tracked_artifacts(root, std::slice::from_ref(path))
                .ok_or("tracked-source revalidation unavailable")?;
            if !tracked.is_empty() {
                return Err("repository changed or artifact contains tracked source".into());
            }
        }
        cursor = root.parent();
    }
    if !ignore_revalidated {
        return Err("required repository/ignore evidence disappeared after review".into());
    }
    Ok(())
}

fn safe_admin_directory(admin: &Path, scope: &Path, home: Option<&Path>) -> Result<bool, String> {
    use crate::fs::{FileSystem, NativeFileSystem, NodeFlags};
    let metadata = NativeFileSystem
        .metadata(admin)
        .map_err(|e| e.to_string())?;
    if metadata.flags.contains(NodeFlags::DATALESS) || metadata.kind == NodeKind::Symlink {
        return Ok(false);
    }
    let directory = if metadata.kind == NodeKind::File {
        let size = std::fs::symlink_metadata(admin)
            .map_err(|e| e.to_string())?
            .len();
        if size > 4096 {
            return Ok(false);
        }
        let text = std::fs::read_to_string(admin).map_err(|e| e.to_string())?;
        let Some(gitdir) = text.trim().strip_prefix("gitdir: ") else {
            return Ok(false);
        };
        admin
            .parent()
            .unwrap()
            .join(gitdir)
            .canonicalize()
            .map_err(|e| e.to_string())?
    } else {
        admin.to_path_buf()
    };
    if !directory.starts_with(scope) {
        return Ok(false);
    }
    let mut prefix = PathBuf::new();
    for component in directory.components() {
        prefix.push(component);
        if std::fs::symlink_metadata(&prefix)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Ok(false);
        }
    }
    let scan = crate::Scanner::new(crate::ScanOptions::default())
        .scan(&directory)
        .map_err(|e| e.to_string())?;
    if !scan.coverage_complete() || !safe_admin_subtree(&scan, scan.arena.root(), home) {
        return Ok(false);
    }
    let Some(common) = common_admin_directory(&directory, scope) else {
        return Ok(false);
    };
    if common != directory {
        let common_scan = crate::Scanner::new(Default::default())
            .scan(&common)
            .map_err(|e| e.to_string())?;
        if !common_scan.coverage_complete()
            || !safe_admin_subtree(&common_scan, common_scan.arena.root(), home)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::SystemProviderHost;
    use crate::targets::{
        BUILTIN_TARGETS, CleanupLevel, PlanSelection, detect_workspace_targets, plan_cleanup,
    };
    use std::fs;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.fsmonitor=false",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--initial-branch=main"]);
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("source.js"), "source").unwrap();
        fs::write(
            dir.path().join(".gitignore"),
            ".next/\nnode_modules/\n.venv/\nbuild/\n.gradle/\n",
        )
        .unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "fixture"]);
        fs::create_dir(dir.path().join(".next")).unwrap();
        fs::write(dir.path().join(".next/output"), "build").unwrap();
        dir
    }
    fn inspect(root: &Path) -> (ProjectInventory, Vec<DetectedTarget>) {
        inspect_with(root, &[], InspectionLimits::default())
    }
    fn inspect_with(
        root: &Path,
        rules: &[RetentionRule],
        limits: InspectionLimits,
    ) -> (ProjectInventory, Vec<DetectedTarget>) {
        let mut scan = crate::Scanner::new(Default::default()).scan(root).unwrap();
        // Deterministic activity fixtures do not change host clocks or require
        // filesystem-specific timestamp tools. Git commits have fixed dates.
        for node in &mut scan.arena.nodes {
            node.modified_unix_seconds = 1;
        }
        let mut targets = detect_workspace_targets(&scan, BUILTIN_TARGETS, None);
        let inventory = inspect_projects_at(
            &scan,
            &mut targets,
            rules,
            &SystemProviderHost::project_inspection(),
            limits,
            1_800_000_000,
        );
        (inventory, targets)
    }
    #[test]
    fn stale_build_is_separate_from_dirty_untracked_and_remote_less_source() {
        let dir = fixture();
        fs::write(dir.path().join("source.js"), "local unsynced work").unwrap();
        fs::write(dir.path().join("new.js"), "untracked work").unwrap();
        let (inventory, targets) = inspect(dir.path());
        let project = &inventory.projects[0];
        let git = project.git.as_ref().unwrap();
        assert!(git.dirty && git.untracked && git.complete);
        assert_eq!(git.synchronization, Synchronization::RemoteLess);
        assert_eq!(project.activity, ProjectActivity::Active);
        assert!(project.artifacts[0].recommended);
        let planned = plan_cleanup(targets, CleanupLevel::Standard, &[], false);
        assert!(
            planned
                .iter()
                .any(|p| matches!(p.selection, PlanSelection::Selected { .. }))
        );
        assert!(
            planned
                .iter()
                .all(|p| p.target.path.as_deref() != Some(dir.path()))
        );
    }
    #[test]
    fn synchronization_states_preserve_no_upstream_unpushed_behind_and_diverged() {
        let mut evidence = GitEvidence::default();
        parse_status("# branch.head main\0", &mut evidence);
        assert_eq!(evidence.synchronization, Synchronization::NoUpstream);
        for (counts, expected) in [
            ("+0 -0", Synchronization::Synchronized),
            ("+2 -0", Synchronization::Unpushed),
            ("+0 -2", Synchronization::Behind),
            ("+2 -2", Synchronization::Diverged),
        ] {
            let mut evidence = GitEvidence::default();
            parse_status(
                &format!("# branch.upstream origin/main\0# branch.ab {counts}\0"),
                &mut evidence,
            );
            assert_eq!(evidence.synchronization, expected);
        }
        // A rename's original filename can begin with a status prefix.
        let mut evidence = GitEvidence::default();
        parse_status("2 rename-record\0? old-name\0", &mut evidence);
        assert!(evidence.dirty && !evidence.untracked);
    }
    #[test]
    fn synchronized_and_unpushed_repositories_are_inspected_without_fetch() {
        let dir = fixture();
        git(
            dir.path(),
            &["remote", "add", "origin", "/nonexistent/no-network"],
        );
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        git(
            dir.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        let (inventory, _) = inspect(dir.path());
        assert_eq!(
            inventory.projects[0].git.as_ref().unwrap().synchronization,
            Synchronization::Synchronized
        );
        assert_eq!(
            inventory.projects[0].activity,
            ProjectActivity::DormantCandidate
        );
        fs::write(dir.path().join("source.js"), "committed new work").unwrap();
        git(dir.path(), &["commit", "-am", "local commit"]);
        let (inventory, _) = inspect(dir.path());
        assert_eq!(
            inventory.projects[0].git.as_ref().unwrap().synchronization,
            Synchronization::Unpushed
        );
        assert!(inventory.projects[0].artifacts[0].recommended);
    }
    #[test]
    fn tracked_generated_names_are_refused_even_when_named_manually() {
        let dir = fixture();
        git(dir.path(), &["add", "--force", ".next/output"]);
        git(dir.path(), &["commit", "-m", "tracked build"]);
        let (inventory, targets) = inspect(dir.path());
        assert!(!inventory.projects[0].artifacts[0].recommended);
        assert!(!targets[0].manually_selectable());
        assert!(matches!(targets[0].safety, CandidateSafety::Refused(_)));
    }
    #[test]
    fn retention_uses_specific_project_over_root_and_keep_requires_manual_naming() {
        let dir = fixture();
        let mut rules = vec![
            RetentionRule {
                path: dir.path().parent().unwrap().into(),
                project_only: false,
                artifact_days: 10,
                dormant_days: 120,
                keep: false,
            },
            RetentionRule {
                path: dir.path().into(),
                project_only: true,
                artifact_days: 20,
                dormant_days: 180,
                keep: true,
            },
        ];
        let (inventory, targets) = inspect_with(dir.path(), &rules, Default::default());
        assert!(inventory.projects[0].protected);
        assert_eq!(targets[0].stale_after_days, 20);
        assert_eq!(targets[0].stale, Some(false));
        assert!(targets[0].manually_selectable());
        rules.pop();
        let (inventory, targets) = inspect_with(dir.path(), &rules, Default::default());
        assert_eq!(inventory.projects[0].dormant_after_days, 120);
        assert_eq!(targets[0].stale_after_days, 10);
    }
    #[test]
    fn budget_exhaustion_is_unknown_and_cannot_authorize_artifacts() {
        let dir = fixture();
        let limits = InspectionLimits {
            repositories: 0,
            ..Default::default()
        };
        let (inventory, targets) = inspect_with(dir.path(), &[], limits);
        assert_eq!(inventory.omitted_repositories, 1);
        assert_eq!(inventory.inspection_commands, 0);
        assert_eq!(inventory.projects[0].activity, ProjectActivity::Unknown);
        assert!(!targets[0].manually_selectable());
        let (_, targets) = inspect_with(
            dir.path(),
            &[],
            InspectionLimits {
                commands: 1,
                ..Default::default()
            },
        );
        assert!(!targets[0].manually_selectable());
    }
    #[test]
    fn recent_source_and_recent_build_weaken_dormancy_independently() {
        let dir = fixture();
        for changed in ["source.js", "output"] {
            let mut scan = crate::Scanner::new(Default::default())
                .scan(dir.path())
                .unwrap();
            let names: Vec<_> = (0..scan.arena.nodes.len())
                .map(|i| scan.arena.name(NodeIndex::from_usize(i)).to_owned())
                .collect();
            for (node, name) in scan.arena.nodes.iter_mut().zip(names) {
                node.modified_unix_seconds = if name == changed { 1_799_999_000 } else { 1 };
            }
            let mut targets = detect_workspace_targets(&scan, BUILTIN_TARGETS, None);
            let inventory = inspect_projects_at(
                &scan,
                &mut targets,
                &[],
                &SystemProviderHost::project_inspection(),
                Default::default(),
                1_800_000_000,
            );
            assert_eq!(
                inventory.projects[0].activity,
                ProjectActivity::Active,
                "{changed}"
            );
        }
    }
    #[test]
    fn worktrees_and_external_worktrees_weaken_dormancy() {
        let scope = tempfile::tempdir().unwrap();
        let main = scope.path().join("main");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "--initial-branch=main"]);
        fs::write(main.join("source"), "fixture").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-m", "fixture"]);
        let linked = scope.path().join("linked");
        git(
            &main,
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );
        fs::write(linked.join("untracked"), "active worktree").unwrap();
        let (inventory, _) = inspect(scope.path());
        assert_eq!(inventory.inspected_repositories, 2);
        assert!(
            inventory
                .projects
                .iter()
                .all(|p| p.activity == ProjectActivity::Active)
        );
        let (inventory, _) = inspect(&main);
        assert_eq!(inventory.projects[0].activity, ProjectActivity::Unknown);
        assert_eq!(
            inventory.projects[0]
                .git
                .as_ref()
                .unwrap()
                .outside_scope_worktrees,
            [linked]
        );
    }
    #[test]
    fn python_environments_require_network_policy_and_ecosystems_are_attributed() {
        let dir = fixture();
        fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();
        fs::write(dir.path().join("build.gradle.kts"), "").unwrap();
        fs::create_dir(dir.path().join(".venv")).unwrap();
        fs::write(
            dir.path().join(".venv/pyvenv.cfg"),
            "home = /fixture/python",
        )
        .unwrap();
        fs::create_dir(dir.path().join("build")).unwrap();
        fs::create_dir(dir.path().join(".gradle")).unwrap();
        fs::create_dir(dir.path().join("Example.xcodeproj")).unwrap();
        let (inventory, targets) = inspect(dir.path());
        assert!(
            inventory.projects[0]
                .artifacts
                .iter()
                .any(|a| a.ecosystem == "python")
        );
        assert!(
            inventory.projects[0]
                .artifacts
                .iter()
                .any(|a| a.ecosystem == "gradle" || a.ecosystem == "xcode")
        );
        assert!(
            targets
                .iter()
                .filter(|target| target.id == "python.venv")
                .all(|target| target.class == crate::targets::TargetClass::Network)
        );
        {
            let planned = plan_cleanup(targets.clone(), CleanupLevel::Safe, &[], false);
            assert!(
                planned
                    .iter()
                    .filter(|t| t.target.id.starts_with("python."))
                    .all(|p| matches!(p.selection, PlanSelection::Skipped(_)))
            );
        }
    }
    struct CargoGitHost {
        root: PathBuf,
    }
    impl ProviderHost for CargoGitHost {
        fn resolve_executable(
            &self,
            name: &str,
        ) -> Result<Option<PathBuf>, crate::providers::ProviderError> {
            if name == "cargo" {
                Ok(Some(PathBuf::from("/fixture/cargo")))
            } else {
                SystemProviderHost::project_inspection().resolve_executable(name)
            }
        }
        fn run_inspection(
            &self,
            command: &CommandSpec,
        ) -> Result<crate::providers::CommandOutput, crate::providers::ProviderError> {
            if command.executable == Path::new("/fixture/cargo") {
                Ok(crate::providers::CommandOutput {
                    exit_code: Some(0),
                    stderr: String::new(),
                    stdout: if command.args == ["--version"] {
                        "cargo fixture".into()
                    } else {
                        serde_json::json!({"workspace_root": self.root, "target_directory": self.root.join("target")}).to_string()
                    },
                })
            } else {
                SystemProviderHost::project_inspection().run_inspection(command)
            }
        }
    }
    #[test]
    fn cargo_recommendations_keep_unsynced_source_separate_in_every_repository_state() {
        for state in [
            "dirty",
            "untracked",
            "remote-less",
            "unpushed",
            "synchronized",
        ] {
            let dir = fixture();
            fs::write(dir.path().join("Cargo.toml"), "fixture manifest").unwrap();
            fs::write(dir.path().join(".gitignore"), ".next/\ntarget/\n").unwrap();
            git(dir.path(), &["add", "."]);
            git(dir.path(), &["commit", "-m", "cargo project"]);
            fs::create_dir(dir.path().join("target")).unwrap();
            fs::write(dir.path().join("target/output"), "generated output").unwrap();
            if matches!(state, "unpushed" | "synchronized") {
                git(
                    dir.path(),
                    &["remote", "add", "origin", "/fixture/no-network"],
                );
                git(
                    dir.path(),
                    &["update-ref", "refs/remotes/origin/main", "HEAD"],
                );
                git(
                    dir.path(),
                    &["branch", "--set-upstream-to=origin/main", "main"],
                );
            }
            match state {
                "dirty" => fs::write(dir.path().join("source.js"), "dirty work").unwrap(),
                "untracked" => fs::write(dir.path().join("local.rs"), "untracked work").unwrap(),
                "unpushed" => {
                    fs::write(dir.path().join("source.js"), "unpushed work").unwrap();
                    git(dir.path(), &["commit", "-am", "local"]);
                }
                _ => {}
            }
            let host = CargoGitHost {
                root: dir.path().canonicalize().unwrap(),
            };
            let mut scan = crate::Scanner::new(Default::default())
                .scan(dir.path())
                .unwrap();
            for node in &mut scan.arena.nodes {
                node.modified_unix_seconds = 1;
            }
            let mut targets = crate::providers::inspect_cargo_builds(&scan, &host, None, 14).items;
            let inventory = inspect_projects_at(
                &scan,
                &mut targets,
                &[],
                &host,
                Default::default(),
                1_800_000_000,
            );
            assert!(inventory.projects[0].artifacts[0].recommended, "{state}");
            assert!(
                inventory.projects[0].artifacts[0]
                    .target
                    .path
                    .as_ref()
                    .unwrap()
                    .ends_with("target")
            );
            assert!(
                inventory.projects[0].recommendation.contains("source")
                    || inventory.projects[0]
                        .recommendation
                        .contains("explicit-only")
            );
            assert!(
                matches!(
                    plan_cleanup(targets, CleanupLevel::Aggressive, &[], false)[0].selection,
                    PlanSelection::Selected { .. }
                ),
                "{state}"
            );
            assert!(dir.path().join("source.js").exists());
        }
    }
    #[test]
    fn cloud_metadata_and_dataless_git_entries_never_launch_git() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("Library/CloudStorage/project");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "must not read").unwrap();
        let scan = crate::Scanner::new(Default::default())
            .scan(home.path())
            .unwrap();
        let inventory = inspect_projects(
            &scan,
            &mut [],
            &[],
            &SystemProviderHost::project_inspection(),
            InspectionLimits {
                home: Some(home.path().into()),
                ..Default::default()
            },
        );
        assert_eq!(inventory.inspection_commands, 0);
        let dir = fixture();
        let mut scan = crate::Scanner::new(Default::default())
            .scan(dir.path())
            .unwrap();
        let index = (0..scan.arena.nodes.len())
            .map(NodeIndex::from_usize)
            .find(|i| scan.arena.name(*i) == ".git")
            .unwrap();
        scan.arena.nodes[index.as_usize()]
            .flags
            .insert(crate::fs::NodeFlags::DATALESS);
        let inventory = inspect_projects(
            &scan,
            &mut [],
            &[],
            &SystemProviderHost::project_inspection(),
            Default::default(),
        );
        assert_eq!(inventory.inspection_commands, 0);
        scan.arena.nodes[index.as_usize()]
            .flags
            .remove(crate::fs::NodeFlags::DATALESS);
        let source = (0..scan.arena.nodes.len())
            .map(NodeIndex::from_usize)
            .find(|i| scan.arena.name(*i) == "source.js")
            .unwrap();
        scan.arena.nodes[source.as_usize()]
            .flags
            .insert(crate::fs::NodeFlags::DATALESS);
        let inventory = inspect_projects(
            &scan,
            &mut [],
            &[],
            &SystemProviderHost::project_inspection(),
            Default::default(),
        );
        assert_eq!(inventory.inspection_commands, 0);
    }

    #[test]
    fn git_filters_and_external_includes_cannot_execute_during_inspection() {
        let dir = fixture();
        let marker = dir.path().join("filter-must-not-run");
        fs::write(
            dir.path().join(".gitattributes"),
            "source.js filter=fixture\n",
        )
        .unwrap();
        git(
            dir.path(),
            &[
                "config",
                "filter.fixture.clean",
                &format!("touch {}", marker.display()),
            ],
        );
        git(
            dir.path(),
            &[
                "config",
                "filter.fixture.process",
                &format!("touch {}", marker.display()),
            ],
        );
        git(dir.path(), &["config", "filter.fixture.required", "true"]);
        fs::write(
            dir.path().join("source.js"),
            "changed source triggers filter unless disabled",
        )
        .unwrap();
        let (inventory, targets) = inspect(dir.path());
        assert!(inventory.projects[0].git.as_ref().unwrap().complete);
        assert!(inventory.projects[0].git.as_ref().unwrap().dirty);
        assert!(!marker.exists());
        assert!(
            revalidate_artifact_repository(
                &targets[0],
                dir.path(),
                None,
                &SystemProviderHost::project_inspection()
            )
            .is_ok()
        );
        assert!(!marker.exists());
        git(
            dir.path(),
            &[
                "config",
                "include.path",
                "/fixture/must-not-read-external-config",
            ],
        );
        let (inventory, targets) = inspect(dir.path());
        assert!(!inventory.projects[0].git.as_ref().unwrap().complete);
        assert_eq!(inventory.inspection_commands, 1);
        assert!(!targets[0].manually_selectable());
    }
    #[test]
    fn selected_review_uses_tracked_metadata_without_running_status_or_claiming_activity() {
        let dir = fixture();
        let (inventory, targets) = inspect_with(
            dir.path(),
            &[],
            InspectionLimits {
                review_only: true,
                ..Default::default()
            },
        );
        let git = inventory.projects[0].git.as_ref().unwrap();
        assert!(git.complete && !git.activity_inspected);
        assert_eq!(inventory.inspection_commands, 2);
        assert!(targets[0].manually_selectable());
        assert_eq!(inventory.projects[0].activity, ProjectActivity::Unknown);
    }
    #[test]
    fn future_artifact_timestamps_cannot_establish_dormancy() {
        let dir = fixture();
        let mut scan = crate::Scanner::new(Default::default())
            .scan(dir.path())
            .unwrap();
        let output = (0..scan.arena.nodes.len())
            .map(NodeIndex::from_usize)
            .find(|i| scan.arena.name(*i) == "output")
            .unwrap();
        for node in &mut scan.arena.nodes {
            node.modified_unix_seconds = 1;
        }
        scan.arena.nodes[output.as_usize()].modified_unix_seconds = 1_900_000_000;
        let mut targets = detect_workspace_targets(&scan, BUILTIN_TARGETS, None);
        let inventory = inspect_projects_at(
            &scan,
            &mut targets,
            &[],
            &SystemProviderHost::project_inspection(),
            Default::default(),
            1_800_000_000,
        );
        assert_eq!(inventory.projects[0].activity, ProjectActivity::Unknown);
        assert!(!inventory.projects[0].artifacts[0].recommended);
    }

    #[test]
    fn revalidation_detects_newly_tracked_output_and_never_runs_fsmonitor_hook() {
        let dir = fixture();
        let marker = dir.path().join("hook-must-not-run");
        git(
            dir.path(),
            &[
                "config",
                "core.fsmonitor",
                &format!("touch {}", marker.display()),
            ],
        );
        let (_, targets) = inspect(dir.path());
        let host = SystemProviderHost::project_inspection();
        assert!(revalidate_artifact_repository(&targets[0], dir.path(), None, &host).is_ok());
        git(dir.path(), &["add", "--force", ".next/output"]);
        assert!(revalidate_artifact_repository(&targets[0], dir.path(), None, &host).is_err());
        assert!(!marker.exists());
    }
}
