//! Pure-state/render fixtures plus sandboxed shared-pipeline equivalence and
//! execution. No fixture reads or deletes the developer's home directory.
use super::{
    app::{Action, App, Run, RunAction, Screen, review_group},
    keymap::Keymap,
    theme::Theme,
    views,
    worker::{self, PlanPurpose, Request, RunEvent, WorkerEvent, Workers},
};
use crate::pipeline::{self, PlanEvent, PlanObserver, ReviewedAction, ReviewedPlan};
use cleanrs_core::{
    accounting::{Confidence, ReclaimEstimate},
    execution::{ActionResult, ActionStatus},
    fs::{NodeKind, ScanProgress},
    targets::*,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::{TempDir, tempdir};

struct Quiet;
impl PlanObserver for Quiet {
    fn on_event(&self, _: PlanEvent) {}
    fn on_scan_progress(&self, _: ScanProgress) {}
}
fn estimate() -> ReclaimEstimate {
    ReclaimEstimate {
        floor: Some(4096),
        ceiling: Some(8192),
        confidence: Confidence::Medium,
        explanation: Some("fixture clone uncertainty".into()),
    }
}
fn candidate(root: &Path, name: &str, class: TargetClass) -> DetectedTarget {
    DetectedTarget {
        storage: Vec::new(),
        cargo_ownership: None,
        gitignore: None,
        id: name.into(),
        provider: ProviderIdentity::workspace(),
        description: name.into(),
        project_root: Some(root.join(name)),
        path: Some(root.join(name).join("cache")),
        class,
        properties: class.properties(),
        minimum_level: CleanupLevel::Safe,
        usage: UsageEstimate::Filesystem {
            apparent: 8192,
            allocated: 8192,
            reclaimable: estimate(),
        },
        age_days: Some(30),
        stale_after_days: 14,
        stale: Some(true),
        safety: CandidateSafety::Eligible,
        operation: PlannedOperation::DirectDelete {
            path: root.join(name).join("cache"),
            children_only: false,
            identity: CandidateIdentity {
                device: 1,
                inode: 1,
                kind: NodeKind::Directory,
            },
        },
        exclusive_group: None,
        exclusive_priority: 0,
        consequence: "regenerate with fixture build".into(),
        evidence: Vec::new(),
    }
}
fn fabricated(root: &Path) -> ReviewedPlan {
    let eligible = candidate(root, "a.eligible", TargetClass::Free);
    let mut standard = candidate(root, "b.standard", TargetClass::Network);
    standard.minimum_level = CleanupLevel::Standard;
    let mut aggressive = candidate(root, "c.aggressive", TargetClass::Expensive);
    aggressive.minimum_level = CleanupLevel::Aggressive;
    let mut fresh = candidate(root, "d.fresh", TargetClass::Free);
    fresh.stale = Some(false);
    fresh.age_days = Some(6);
    let explicit = candidate(root, "e.user_data", TargetClass::Irreplaceable);
    let mut refused = candidate(root, "f.refused", TargetClass::Free);
    refused.safety = CandidateSafety::Refused("outside scope".into());
    refused.usage = UsageEstimate::Unknown {
        explanation: "outside scope".into(),
    };
    let mut info = candidate(root, "g.informational", TargetClass::Irreplaceable);
    info.operation = PlannedOperation::Informational;
    let targets = plan_cleanup(
        vec![
            eligible, standard, aggressive, fresh, explicit, refused, info,
        ],
        CleanupLevel::Safe,
        &[],
        false,
    );
    let actions = targets
        .iter()
        .filter(|r| matches!(r.selection, PlanSelection::Selected { .. }))
        .map(|r| ReviewedAction {
            target: r.target.clone(),
            selection_source: SelectionSource::Level,
            executable_identity: None,
            executable_identity_error: None,
        })
        .collect();
    let mut arena = cleanrs_core::fs::Arena::new(root);
    for (i, row) in targets.iter().enumerate() {
        if !matches!(row.target.usage, UsageEstimate::Filesystem { .. }) {
            continue;
        }
        let mut directory = cleanrs_core::fs::Node::default();
        directory.kind = NodeKind::Directory;
        let project = arena.push_child(arena.root(), &row.target.id, directory.clone());
        let cache = arena.push_child(project, "cache", directory);
        let mut file = cleanrs_core::fs::Node::default();
        file.kind = NodeKind::File;
        file.device = 1;
        file.inode = i as u64 + 2;
        file.allocated_size = 8192;
        file.private_size = Some(4096);
        arena.push_child(cache, "payload", file);
    }
    let accounting = Arc::new(pipeline::SelectionAccounting::new(arena, true, &targets));
    ReviewedPlan {
        system: false,
        projects: Default::default(),
        scan_finished: None,
        scan_age_offset: Duration::ZERO,
        accounting,
        coverage_complete: true,
        providers: Vec::new(),
        selected_provider_actions: 0,
        selected_unknown_actions: 0,
        run_id: "fixture-run".into(),
        created_unix_seconds: 0,
        scope: root.into(),
        level: CleanupLevel::Safe,
        explicitly_named_ids: Vec::new(),
        targets,
        selected_estimate: estimate(),
        selected_provider_reported: 0,
        actions,
    }
}
fn fixture() -> (TempDir, App) {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut app = App::new(root.clone());
    app.home = Some(root.clone());
    app.drain_requests();
    app.handle(Action::Targets);
    let generation = app.targets().unwrap().generation;
    app.drain_requests();
    app.on_worker(WorkerEvent::Planned {
        scope: root.clone(),
        generation,
        purpose: PlanPurpose::Detect,
        result: Ok(Arc::new(fabricated(&root))),
    });
    (dir, app)
}
fn manual_plan(app: &App) -> Arc<ReviewedPlan> {
    let t = app.targets().unwrap();
    let mut plan = (**t.baseline.as_ref().unwrap()).clone();
    plan.targets = plan_cleanup_with_selection(
        plan.targets.into_iter().map(|r| r.target).collect(),
        t.level,
        &[],
        false,
        &t.selected,
        SelectionMode::ManualOnly,
    )
    .unwrap();
    plan.actions = plan
        .targets
        .iter()
        .filter_map(|r| match &r.selection {
            PlanSelection::Selected { source } => Some(ReviewedAction {
                target: r.target.clone(),
                selection_source: source.clone(),
                executable_identity: None,
                executable_identity_error: None,
            }),
            _ => None,
        })
        .collect();
    Arc::new(plan)
}
fn review(app: &mut App) {
    app.handle(Action::Review);
    let plan = manual_plan(app);
    let scope = app.target_scope.clone().unwrap();
    let generation = app.targets().unwrap().generation;
    app.on_worker(WorkerEvent::Planned {
        scope,
        generation,
        purpose: PlanPurpose::Review,
        result: Ok(plan),
    });
}
fn run_fixture(app: &mut App) {
    let plan = manual_plan(app);
    app.run = Some(Run {
        actions: vec![RunAction::Pending; plan.actions.len()],
        plan,
        stop: Arc::new(AtomicBool::new(false)),
        confirm_stop: false,
        output: VecDeque::new(),
        output_scroll: 0,
        scroll: 0,
        summary: None,
        error: None,
    });
    app.screens.push(Screen::Run);
}
fn press(app: &mut App, map: &mut Keymap, code: KeyCode, mods: KeyModifiers) {
    let key = KeyEvent::new(code, mods);
    let action = if app.confirm_quit {
        map.quit_action(key)
    } else {
        map.action(key, app.screen(), app.filtering)
    };
    if let Some(action) = action {
        app.handle(action);
    }
}
#[test]
fn targets_scope_cache_and_default_request_match_cli() {
    let (_dir, mut app) = fixture();
    assert_eq!(app.targets().unwrap().selected.len(), 1);
    app.handle(Action::Toggle);
    assert!(app.targets().unwrap().selected.is_empty());
    app.handle(Action::Back);
    app.handle(Action::Targets);
    assert!(app.targets().unwrap().selected.is_empty());
    app.handle(Action::SystemTargets);
    let requests = app.drain_requests();
    let Request::Plan { request, .. } = requests.last().unwrap() else {
        panic!("plan request");
    };
    assert!(!request.workspace_only);
    assert_eq!(request.level, CleanupLevel::Safe);
    assert!(
        request.config.is_none() && request.target.is_empty() && request.target_argv.is_empty()
    );
    app.handle(Action::Back);
    app.handle(Action::Rescan);
    assert!(
        matches!(app.drain_requests().last(), Some(Request::Plan { request, .. }) if request.workspace_only)
    );
}
#[test]
fn keys_toggle_visual_all_invert_clear_and_presets_keep_refusals_unticked() {
    let (_dir, mut app) = fixture();
    let mut map = Keymap::default();
    press(&mut app, &mut map, KeyCode::Char('v'), KeyModifiers::NONE);
    press(&mut app, &mut map, KeyCode::Char('j'), KeyModifiers::NONE);
    press(&mut app, &mut map, KeyCode::Char('v'), KeyModifiers::NONE);
    assert_eq!(app.targets().unwrap().selected.len(), 2);
    press(&mut app, &mut map, KeyCode::Char('u'), KeyModifiers::NONE);
    assert!(app.targets().unwrap().selected.is_empty());
    press(
        &mut app,
        &mut map,
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
    );
    assert_eq!(app.targets().unwrap().selected.len(), 1);
    press(
        &mut app,
        &mut map,
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    );
    assert_eq!(app.targets().unwrap().selected.len(), 4);
    let untickable: Vec<_> = app
        .target_rows()
        .into_iter()
        .filter(|r| !r.target.manually_selectable())
        .map(|r| r.candidate_key.clone())
        .collect();
    for key in &untickable {
        app.targets_cache
            .get_mut(app.target_scope.as_ref().unwrap())
            .unwrap()
            .highlighted = Some(key.clone());
        press(&mut app, &mut map, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!app.targets().unwrap().selected.contains(key));
    }
    app.handle(Action::First);
    app.handle(Action::Visual);
    app.handle(Action::Last);
    app.handle(Action::Toggle);
    assert_eq!(app.targets().unwrap().selected.len(), 5);
    assert!(
        untickable
            .iter()
            .all(|k| !app.targets().unwrap().selected.contains(k))
    );
    app.handle(Action::Visual);
    app.handle(Action::Back);
    assert_eq!(app.screen(), Screen::Targets);
    assert!(app.targets().unwrap().visual.is_none());
    for (key, level, count) in [
        ('1', CleanupLevel::Safe, 1),
        ('2', CleanupLevel::Standard, 2),
        ('3', CleanupLevel::Aggressive, 3),
    ] {
        press(&mut app, &mut map, KeyCode::Char(key), KeyModifiers::NONE);
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .0
                .contains("manual ticks discarded")
        );
        assert_eq!(app.targets().unwrap().level, level);
        assert!(
            app.drain_requests().is_empty(),
            "presets must use cached detection"
        );
        assert_eq!(app.targets().unwrap().selected.len(), count);
        assert!(
            untickable
                .iter()
                .all(|k| !app.targets().unwrap().selected.contains(k))
        );
    }
}
#[test]
fn review_groups_confirmation_and_cancel_keep_ticks() {
    let (_dir, mut app) = fixture();
    app.handle(Action::ClearSelection);
    app.handle(Action::Review);
    assert_eq!(app.screen(), Screen::Targets);
    assert!(app.status.as_ref().unwrap().0.contains("at least one"));
    app.handle(Action::Invert);
    review(&mut app);
    assert_eq!(app.screen(), Screen::Review);
    let groups: HashSet<_> = app
        .reviewed
        .as_ref()
        .unwrap()
        .actions
        .iter()
        .map(|a| review_group(&a.selection_source, a.target.explicit_only()))
        .collect();
    assert_eq!(groups, HashSet::from([0, 1, 2]));
    assert!(
        app.reviewed
            .as_ref()
            .unwrap()
            .actions
            .iter()
            .all(|a| matches!(a.selection_source, SelectionSource::Manual { .. }))
    );
    let selected = app.targets().unwrap().selected.clone();
    let mut map = Keymap::default();
    app.drain_requests();
    press(&mut app, &mut map, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.screen(), Screen::Review);
    assert!(app.drain_requests().is_empty());
    press(&mut app, &mut map, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.targets().unwrap().selected, selected);
    review(&mut app);
    app.drain_requests();
    press(&mut app, &mut map, KeyCode::Char('y'), KeyModifiers::NONE);
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        assert_eq!(app.screen(), Screen::Review);
        return;
    }
    assert_eq!(app.screen(), Screen::Run);
    assert!(matches!(
        app.drain_requests().last(),
        Some(Request::Execute { .. })
    ));
}
#[test]
fn stale_rebuilds_never_become_reviewable_and_stop_is_between_actions() {
    let (_dir, mut app) = fixture();
    app.handle(Action::Review);
    let generation = app.targets().unwrap().generation;
    let plan = manual_plan(&app);
    app.handle(Action::Toggle);
    app.on_worker(WorkerEvent::Planned {
        scope: app.target_scope.clone().unwrap(),
        generation,
        purpose: PlanPurpose::Review,
        result: Ok(plan),
    });
    assert_eq!(app.screen(), Screen::Targets);
    assert!(app.targets().unwrap().summary.is_some());
    run_fixture(&mut app);
    let mut map = Keymap::default();
    press(
        &mut app,
        &mut map,
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    );
    assert!(app.run.as_ref().unwrap().confirm_stop);
    assert!(!app.quit);
    press(&mut app, &mut map, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(!app.run.as_ref().unwrap().confirm_stop);
    press(&mut app, &mut map, KeyCode::Char('q'), KeyModifiers::NONE);
    press(&mut app, &mut map, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(app.run.as_ref().unwrap().stop.load(Ordering::Acquire));
    assert!(!app.quit);
}
fn rendered(app: &App, colors: bool, width: u16, height: u16) -> (String, ratatui::buffer::Buffer) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| views::render(f, app, Theme { colors }))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text = buffer
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    if !colors {
        assert!(
            buffer
                .content
                .iter()
                .all(|c| c.fg == Color::Reset && c.bg == Color::Reset)
        );
    }
    (text, buffer)
}
#[test]
fn selected_target_details_show_full_names_and_wrapped_explanations() {
    let (_dir, mut app) = fixture();
    let mut plan = fabricated(app.target_scope.as_ref().unwrap());
    plan.targets.truncate(1);
    let target = &mut plan.targets[0].target;
    target.id = "docker.volumes-anonymous".into();
    target.consequence = "This provider explanation is deliberately long enough to wrap on an eighty column terminal while keeping the reclaimable confidence visible below it.".into();
    let generation = app.targets().unwrap().generation;
    app.on_worker(WorkerEvent::Planned {
        scope: app.target_scope.clone().unwrap(),
        generation,
        purpose: PlanPurpose::Detect,
        result: Ok(Arc::new(plan)),
    });
    for (width, height) in [(80, 24), (100, 28), (150, 32)] {
        let (_, buffer) = rendered(&app, false, width, height);
        // Ignore line padding when checking text that wraps across terminal rows.
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for expected in [
            "Target: docker.volumes-anonymous",
            "delete directory",
            "recoverability: generated",
            "Occupied",
            "keeping the reclaimable confidence visible below it.",
            "Reclaimable",
            "confidence Medium",
            "q back",
        ] {
            assert!(
                text.contains(expected),
                "missing {expected} at {width}x{height}: {text}"
            );
        }
    }
}
#[test]
fn all_screens_render_semantic_states_and_warnings_without_color() {
    for colors in [false, true] {
        let (_dir, mut app) = fixture();
        app.handle(Action::Move(1));
        app.handle(Action::Toggle);
        let text = rendered(&app, colors, 150, 32).0;
        for expected in [
            "[x]",
            "[ ]",
            "[!]",
            "✗",
            "needs standard",
            "needs aggressive",
            "not stale (14d)",
            "explicit only",
            "refused: outside scope",
            "delete directory",
            "recoverability:",
            "Selected 2",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        let narrow = rendered(&app, colors, 80, 24).0;
        assert!(
            narrow.contains("level: safe")
                && narrow.contains("scope: folder")
                && narrow.contains("q back")
        );
        assert!(
            rendered(&app, colors, 150, 32)
                .0
                .contains("8.0 KiB–16.0 KiB")
        );
        app.handle(Action::ClearSelection);
        app.handle(Action::Invert);
        review(&mut app);
        let text = rendered(&app, colors, 150, 50).0;
        for expected in [
            "Selected by level",
            "Manually selected — not eligible by default",
            "These items would be skipped at level safe",
            "Explicit-only / user data",
            "irreplaceable",
            "Permanent deletion — not moved to Trash",
            "provider estimates",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        app.screens.pop();
        run_fixture(&mut app);
        let run = app.run.as_mut().unwrap();
        run.actions = vec![
            RunAction::Pending,
            RunAction::Running,
            RunAction::Finished(ActionResult::failed("fixture failure")),
            RunAction::Finished(ActionResult::validation_abort("fixture replacement")),
            RunAction::Finished(ActionResult {
                status: ActionStatus::Skipped,
                error: Some("stopped by user".into()),
                ..ActionResult::failed("")
            }),
        ];
        run.output.push_back("2 Stdout: fixture output".into());
        let text = rendered(&app, colors, 150, 40).0;
        for expected in [
            "pending",
            "running",
            "failed: fixture failure",
            "aborted · changed since scan",
            "skipped: stopped by user",
            "fixture output",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        let success = ActionResult {
            status: ActionStatus::Success,
            removed_allocation: Some(8192),
            provider_reported: None,
            ..ActionResult::failed("")
        };
        app.run.as_mut().unwrap().actions[0] = RunAction::Finished(success.clone());
        assert!(
            rendered(&app, colors, 150, 40)
                .0
                .contains("done · removed allocation 8.0 KiB")
        );
        app.run.as_mut().unwrap().actions[0] = RunAction::Finished(ActionResult {
            removed_allocation: None,
            provider_reported: Some(2048),
            ..success
        });
        assert!(
            rendered(&app, colors, 150, 40)
                .0
                .contains("done · provider reported 2.0 KiB")
        );
        app.handle(Action::Back);
        assert!(
            rendered(&app, colors, 80, 24)
                .0
                .contains("Stop after the current action? y/n")
        );
        let run = app.run.as_mut().unwrap();
        let actions = run
            .plan
            .actions
            .iter()
            .enumerate()
            .map(|(i, a)| cleanrs_core::journal::JournalAction {
                evidence: vec![],
                target_id: a.target.id.clone(),
                provider_id: a.target.provider.id.clone(),
                path: a.target.path.clone(),
                context: None,
                argv: None,
                status: [
                    "success",
                    "failed",
                    "validation_abort",
                    "skipped",
                    "success",
                ][i]
                    .into(),
                selection_source: "manual".into(),
                overridden_skip_reason: None,
                planned_allocated: Some(8192),
                estimated_floor: Some(4096),
                estimated_ceiling: Some(8192),
                provider_estimated: None,
                removed_allocation: (i == 0).then_some(8192),
                provider_reported: (i == 4).then_some(1024),
                bytes_trashed: None,
                exit_code: None,
                stdout_summary: None,
                stderr_summary: None,
                error: None,
            })
            .collect();
        run.summary = Some(cleanrs_core::journal::JournalRun {
            schema: 1,
            run_id: "fixture-run".into(),
            timestamp_unix_seconds: 0,
            cleanrs_version: "fixture".into(),
            scope: run.plan.scope.clone(),
            system: run.plan.system,
            level: "safe".into(),
            explicitly_named_ids: Vec::new(),
            outcome: "partial".into(),
            actions,
        });
        app.screens.pop();
        app.screens.push(Screen::Results);
        let text = rendered(&app, colors, 150, 32).0;
        for expected in [
            "Removed allocation 8.0 KiB",
            "provider reported 1.0 KiB",
            "Failures 1",
            "validation_abort 1",
            "skipped 1",
            "fixture-run",
            "cleanrs history shows details",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        rendered(&app, colors, 80, 24);
        app.handle(Action::Back);
        assert_eq!(app.screen(), Screen::Browser);
        assert!(app.targets().is_none());
        assert!(
            app.drain_requests()
                .iter()
                .any(|r| matches!(r, Request::Rescan { .. }))
        );
    }
}
#[test]
fn pipeline_tick_keys_equal_cli_select_and_preserve_policy_eligible_source() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    for name in ["node_modules", ".next", "dist"] {
        std::fs::create_dir_all(root.join("project").join(name)).unwrap();
        std::fs::write(
            root.join("project").join(name).join("payload"),
            vec![0; 4096],
        )
        .unwrap();
    }
    std::fs::write(root.join("project/package.json"), b"{}").unwrap();
    let mut request = worker::plan_request(
        root.clone(),
        Some(root.join("home")),
        CleanupLevel::Safe,
        false,
        None,
    );
    // Aged fixture verifies no override on level-eligible manual ticks.
    for name in [".next", "dist"] {
        let artifact = root.join("project").join(name);
        for entry in std::fs::read_dir(&artifact).unwrap() {
            std::fs::File::open(entry.unwrap().path())
                .unwrap()
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(40 * 86400))
                .unwrap();
        }
        let file = std::fs::File::open(artifact).unwrap();
        file.set_modified(std::time::SystemTime::now() - Duration::from_secs(40 * 86400))
            .unwrap();
    }
    let baseline = pipeline::build_reviewed_plan(request, Arc::new(Quiet)).unwrap();
    let keys = baseline
        .targets
        .iter()
        .map(|r| r.candidate_key.clone())
        .collect();
    request = worker::plan_request(
        root.clone(),
        Some(root.join("home")),
        CleanupLevel::Safe,
        false,
        Some(keys),
    );
    let tui = pipeline::build_reviewed_plan(request, Arc::new(Quiet)).unwrap();
    let mut cli = worker::plan_request(
        root.clone(),
        Some(root.join("home")),
        CleanupLevel::Safe,
        false,
        Some(HashSet::new()),
    );
    cli.select = baseline
        .targets
        .iter()
        .map(|r| r.target.path.as_ref().unwrap().display().to_string())
        .collect();
    let cli = pipeline::build_reviewed_plan(cli, Arc::new(Quiet)).unwrap();
    assert_eq!(
        serde_json::to_value(&tui.actions).unwrap(),
        serde_json::to_value(&cli.actions).unwrap()
    );
    assert_eq!(tui.selected_estimate, cli.selected_estimate);
    assert_eq!(tui.actions.len(), 3);
    assert!(tui.actions.iter().any(|a| matches!(
        &a.selection_source,
        SelectionSource::Manual { overrode: None }
    )));
    assert!(tui.actions.iter().any(|a| matches!(&a.selection_source, SelectionSource::Manual { overrode: Some(r) } if r == "requires standard cleanup level")));
    assert!(root.join("project/node_modules/payload").exists());
}

/// Execute in a child test process so HOME/XDG overrides cannot race any other
/// test. The entire workspace, config, provider, output and journal are under
/// one parent-owned TempDir; the child never uses the real environment home.
#[test]
#[cfg(unix)]
fn execution_worker_sandbox() {
    if std::env::var_os("CLEANRS_TUI_TEST_CHILD").is_none() {
        let sandbox = tempdir().unwrap();
        let sandbox_root = sandbox.path().canonicalize().unwrap();
        let home = sandbox_root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tui::tests::execution_worker_sandbox",
                "--nocapture",
            ])
            .env("CLEANRS_TUI_TEST_CHILD", &sandbox_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", sandbox_root.join("state"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let sandbox = PathBuf::from(std::env::var_os("CLEANRS_TUI_TEST_CHILD").unwrap())
        .canonicalize()
        .unwrap();
    let home = sandbox.join("home");
    assert_eq!(std::env::var_os("HOME").unwrap(), home.as_os_str());
    let root = sandbox.join("workspace");
    for name in [".next", "dist", "node_modules"] {
        std::fs::create_dir_all(root.join("project").join(name)).unwrap();
        std::fs::write(
            root.join("project").join(name).join("payload"),
            vec![0; 4096],
        )
        .unwrap();
    }
    std::fs::write(root.join("project/package.json"), b"{}").unwrap();
    let root = root.canonicalize().unwrap();
    let baseline = pipeline::build_reviewed_plan(
        worker::plan_request(
            root.clone(),
            Some(home.clone()),
            CleanupLevel::Safe,
            false,
            None,
        ),
        Arc::new(Quiet),
    )
    .unwrap();
    let key = baseline
        .targets
        .iter()
        .find(|r| r.target.id == "node.next")
        .unwrap()
        .candidate_key
        .clone();
    let plan = Arc::new(
        pipeline::build_reviewed_plan(
            worker::plan_request(
                root.clone(),
                Some(home.clone()),
                CleanupLevel::Safe,
                false,
                Some(HashSet::from([key])),
            ),
            Arc::new(Quiet),
        )
        .unwrap(),
    );
    let workers = Workers::start();
    workers
        .requests
        .send(Request::Execute {
            plan: plan.clone(),
            home: Some(home.clone()),
            stop: Arc::new(AtomicBool::new(false)),
        })
        .unwrap();
    let mut events = Vec::new();
    let run = loop {
        let WorkerEvent::Run { run_id, event } = workers
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        else {
            panic!("run event");
        };
        assert_eq!(run_id, plan.run_id);
        match event {
            RunEvent::Started { total } => {
                assert_eq!(total, 1);
                events.push("run");
            }
            RunEvent::ActionStarted { index } => {
                assert_eq!(index, 0);
                events.push("start");
            }
            RunEvent::ActionFinished { index, result } => {
                assert_eq!(index, 0);
                assert_eq!(result.status, ActionStatus::Success);
                events.push("finish");
            }
            RunEvent::Finished(run) => {
                events.push("done");
                break run;
            }
            RunEvent::Error(error) if unsafe { libc::geteuid() } == 0 => {
                assert!(error.contains("root"));
                assert!(root.join("project/.next").exists());
                return;
            }
            RunEvent::Error(error) => panic!("{error}"),
            _ => panic!("unexpected output"),
        }
    };
    assert_eq!(events, ["run", "start", "finish", "done"]);
    assert!(!root.join("project/.next").exists());
    assert!(root.join("project/dist/payload").exists());
    assert!(root.join("project/node_modules/payload").exists());
    let action = run
        .actions
        .iter()
        .find(|a| a.target_id == "node.next")
        .unwrap();
    assert_eq!(action.selection_source, "manual");
    assert_eq!(
        action.overridden_skip_reason.as_deref(),
        Some("target is not stale")
    );
    let journal = pipeline::journal_for_environment(Some(&home)).unwrap();
    assert!(journal.path().starts_with(&sandbox));
    assert_eq!(journal.read().unwrap()[0].run_id, plan.run_id);
    drop(workers);

    // A fixture provider gates completion on a file, ensuring the stop request
    // arrives during an action and proves it finishes without being killed.
    let executable = sandbox.join("fixture-provider");
    let release = sandbox.join("release");
    let marker = sandbox.join("provider-finished");
    std::fs::write(&executable, "#!/bin/sh\nprintf 'provider reclaimed 2MiB\\n'\nprintf 'fixture stderr\\n' >&2\nwhile [ ! -f \"$1\" ]; do sleep 0.01; done\nprintf done > \"$2\"\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = home.join(".config/cleanrs/targets.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let mut content = String::new();
    for id in ["fixture.first", "fixture.second"] {
        content.push_str(&format!("\n[[target]]\nid = {id:?}\nclass = \"irreplaceable\"\ndetect = {{ command = {:?} }}\nreclaim = {{ method = \"run\", argv = [{:?}, {:?}, {:?}] }}\n", executable.display().to_string(),executable.display().to_string(),release.display().to_string(),marker.display().to_string()));
    }
    std::fs::write(config, content).unwrap();
    let baseline = pipeline::build_reviewed_plan(
        worker::plan_request(
            root.clone(),
            Some(home.clone()),
            CleanupLevel::Safe,
            false,
            None,
        ),
        Arc::new(Quiet),
    )
    .unwrap();
    let keys = baseline
        .targets
        .iter()
        .filter(|r| r.target.id.starts_with("fixture."))
        .map(|r| r.candidate_key.clone())
        .collect();
    let plan = Arc::new(
        pipeline::build_reviewed_plan(
            worker::plan_request(
                root,
                Some(home.clone()),
                CleanupLevel::Safe,
                false,
                Some(keys),
            ),
            Arc::new(Quiet),
        )
        .unwrap(),
    );
    assert_eq!(plan.actions.len(), 2);
    let stop = Arc::new(AtomicBool::new(false));
    let workers = Workers::start();
    workers
        .requests
        .send(Request::Execute {
            plan: plan.clone(),
            home: Some(home),
            stop: stop.clone(),
        })
        .unwrap();
    let mut events = Vec::new();
    let mut streams = HashSet::new();
    let run = loop {
        let WorkerEvent::Run { event, .. } = workers
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        else {
            panic!("run event");
        };
        match event {
            RunEvent::Started { total } => {
                assert_eq!(total, 2);
                events.push("run");
            }
            RunEvent::ActionStarted { index } => {
                assert_eq!(index, 0);
                events.push("start");
                stop.store(true, Ordering::Release);
                std::fs::write(&release, b"release").unwrap();
            }
            RunEvent::Output {
                index,
                stream,
                line,
            } => {
                assert_eq!(index, 0);
                assert!(events.contains(&"start") && !events.contains(&"finish"));
                streams.insert(format!("{stream:?}"));
                assert!(line.contains("provider reclaimed") || line.contains("fixture stderr"));
            }
            RunEvent::ActionFinished { index, result } => {
                if index == 0 {
                    assert_eq!(result.status, ActionStatus::Success);
                    assert_eq!(result.provider_reported, Some(2 * 1024 * 1024));
                    events.push("finish");
                } else {
                    assert_eq!(result.status, ActionStatus::Skipped);
                    assert_eq!(result.error.as_deref(), Some("stopped by user"));
                    events.push("skip");
                }
            }
            RunEvent::Finished(run) => {
                events.push("done");
                break run;
            }
            RunEvent::Error(error) => panic!("{error}"),
        }
    };
    assert_eq!(events, ["run", "start", "finish", "skip", "done"]);
    assert_eq!(streams, HashSet::from(["Stdout".into(), "Stderr".into()]));
    assert!(marker.exists());
    for action in run
        .actions
        .iter()
        .filter(|a| a.target_id.starts_with("fixture."))
    {
        assert_eq!(action.selection_source, "manual");
        assert!(action.overridden_skip_reason.is_some());
    }
    let persisted = journal.read().unwrap();
    assert_eq!(persisted.len(), 2);
    assert_eq!(
        serde_json::to_value(&persisted[1]).unwrap(),
        serde_json::to_value(&run).unwrap()
    );
}

#[test]
fn worker_detection_cached_total_and_fresh_review() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("project/.next")).unwrap();
    std::fs::write(root.join("project/package.json"), b"{}").unwrap();
    std::fs::write(root.join("project/.next/payload"), vec![0; 4096]).unwrap();
    std::fs::create_dir(root.join("project/dist")).unwrap();
    std::fs::hard_link(
        root.join("project/.next/payload"),
        root.join("project/dist/shared"),
    )
    .unwrap();
    let workers = Workers::start();
    let mut app = App::new(root.clone());
    app.home = Some(root.clone());
    app.drain_requests();
    app.handle(Action::Targets);
    for request in app.drain_requests() {
        workers.requests.send(request).unwrap();
    }
    let mut activities = Vec::new();
    loop {
        let event = workers
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        if let WorkerEvent::Planning { activity, .. } = &event {
            activities.push(activity.clone());
        }
        let complete = matches!(event, WorkerEvent::Planned { .. });
        app.on_worker(event);
        if complete {
            break;
        }
    }
    assert!(activities.iter().any(|a| a.contains("workspace detection")));
    assert!(activities.iter().any(|a| a.contains("provider inspection")));
    assert!(app.targets().unwrap().selected.is_empty());
    app.handle(Action::Toggle);
    app.on_tick();
    assert!(app.drain_requests().is_empty());
    assert_eq!(
        app.targets()
            .unwrap()
            .summary
            .as_ref()
            .unwrap()
            .estimate
            .ceiling,
        Some(0),
        "one outside hardlink pins the inode"
    );
    app.handle(Action::Toggle);
    let cached = app.targets().unwrap().summary.clone().unwrap();
    assert_eq!(cached.count, 2);
    assert!(
        cached.estimate.ceiling.unwrap() > 0,
        "selecting both names includes the allocation once"
    );
    // Even after the former debounce period and leaving targets, no plan or
    // browse request may be sent. Only x requests authoritative planning.
    app.handle(Action::Back);
    std::thread::sleep(Duration::from_millis(200));
    app.on_tick();
    assert!(app.drain_requests().is_empty());
    app.handle(Action::Targets);
    app.handle(Action::Review);
    for request in app.drain_requests() {
        workers.requests.send(request).unwrap();
    }
    loop {
        let event = workers
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        let complete = matches!(event, WorkerEvent::Planned { .. });
        app.on_worker(event);
        if complete {
            break;
        }
    }
    assert_eq!(app.screen(), Screen::Review);
    let plan = app.reviewed.as_ref().unwrap();
    assert_eq!(plan.actions.len(), 2);
    assert_eq!(cached.estimate, plan.selected_estimate);
    assert_eq!(cached.provider_reported, plan.selected_provider_reported);
    assert_eq!(
        plan.actions[0].selection_source,
        SelectionSource::Manual {
            overrode: Some("target is not stale".into())
        }
    );
    assert!(root.join("project/.next/payload").exists());
}

#[test]
fn core_rejects_refused_ticks_and_status_is_structured() {
    let (_dir, app) = fixture();
    let baseline = app.targets().unwrap().baseline.as_ref().unwrap();
    for row in &baseline.targets {
        if !row.target.manually_selectable() {
            assert!(
                plan_cleanup_with_selection(
                    baseline.targets.iter().map(|r| r.target.clone()).collect(),
                    CleanupLevel::Aggressive,
                    &[],
                    true,
                    &HashSet::from([row.candidate_key.clone()]),
                    SelectionMode::ManualOnly
                )
                .is_err()
            );
        }
    }
    let row = baseline
        .targets
        .iter()
        .find(|r| r.target.id == "c.aggressive")
        .unwrap();
    assert_eq!(
        row.policy_status(CleanupLevel::Safe),
        PolicyStatus::NeedsLevel(CleanupLevel::Aggressive)
    );
    assert_eq!(
        views::targets::policy_label(row, CleanupLevel::Safe),
        "needs aggressive"
    );
}

#[test]
fn redetection_preserves_surviving_ticks_and_narrow_help_scrolls() {
    let (_dir, mut app) = fixture();
    app.handle(Action::Toggle); // Explicitly untick the automatic eligible row.
    app.handle(Action::Toggle); // Explicitly tick the standard-only row.
    let selected = app.targets().unwrap().selected.clone();
    app.handle(Action::Rescan);
    let scope = app.target_scope.clone().unwrap();
    let generation = app.targets().unwrap().generation;
    app.on_worker(WorkerEvent::Planned {
        scope: scope.clone(),
        generation,
        purpose: PlanPurpose::Detect,
        result: Ok(Arc::new(fabricated(&scope))),
    });
    assert_eq!(app.targets().unwrap().selected, selected);
    assert!(app.targets().unwrap().summary.is_some());
    app.handle(Action::Rescan);
    let generation = app.targets().unwrap().generation;
    app.on_worker(WorkerEvent::Planned {
        scope: scope.clone(),
        generation,
        purpose: PlanPurpose::Detect,
        result: Ok(Arc::new(fabricated(&scope))),
    });
    assert_eq!(app.targets().unwrap().selected, selected);
    app.handle(Action::Help);
    app.page_rows = 22;
    app.handle(Action::Last);
    assert!(rendered(&app, false, 80, 24).0.contains("Ctrl-c"));
    let last = app.help_scroll;
    assert!(last > 0);
    app.handle(Action::Move(-1));
    assert_eq!(app.help_scroll, last - 1);
    app.handle(Action::Back);
    assert_eq!(app.screen(), Screen::Targets);
    run_fixture(&mut app);
    app.handle(Action::Help);
    app.handle(Action::Quit);
    assert_eq!(app.screen(), Screen::Run);
    assert!(app.run.as_ref().unwrap().confirm_stop);
    assert!(!app.quit);
    // Provider output is bounded in App as well as sanitized for terminal use.
    let run_id = app.run.as_ref().unwrap().plan.run_id.clone();
    for n in 0..250 {
        app.on_worker(WorkerEvent::Run {
            run_id: run_id.clone(),
            event: RunEvent::Output {
                index: 0,
                stream: cleanrs_core::execution::OutputStream::Stdout,
                line: format!("line {n}\u{1b}\n"),
            },
        });
    }
    assert_eq!(app.run.as_ref().unwrap().output.len(), 200);
    assert!(
        app.run
            .as_ref()
            .unwrap()
            .output
            .front()
            .unwrap()
            .ends_with("line 50")
    );
}

#[test]
fn focused_target_keeps_semantic_selection_color() {
    let (_dir, app) = fixture();
    let buffer = rendered(&app, true, 150, 32).1;
    let mark = buffer
        .content
        .windows(3)
        .find(|cells| {
            cells[0].symbol() == "[" && cells[1].symbol() == "x" && cells[2].symbol() == "]"
        })
        .unwrap();
    assert!(mark.iter().all(|cell| cell.fg == Color::Green));
}

#[test]
fn recognized_target_row_opens_containing_scope_and_focuses_target() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    std::fs::write(root.join("package.json"), b"{}").unwrap();
    for name in ["node_modules", ".next", "dist"] {
        std::fs::create_dir(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("payload"), vec![1; 4096]).unwrap();
    }
    let snapshot = Arc::new(worker::Snapshot::new(
        cleanrs_core::Scanner::new(Default::default())
            .scan(&root)
            .unwrap(),
        "fixture".into(),
    ));
    let plan = Arc::new(
        pipeline::build_reviewed_plan(
            worker::plan_request(
                root.clone(),
                Some(root.join("home")),
                CleanupLevel::Safe,
                false,
                None,
            ),
            Arc::new(Quiet),
        )
        .unwrap(),
    );
    for name in ["node_modules", ".next", "dist"] {
        let mut app = App::new(root.clone());
        app.drain_requests();
        app.on_worker(WorkerEvent::Listed {
            path: root.clone(),
            revision: 0,
            rows: snapshot.rows(snapshot.scan.arena.root()),
            total: 0,
            volume: "fixture".into(),
        });
        app.directories.get_mut(&root).unwrap().selected = Some(root.join(name));
        app.handle(Action::Targets);
        assert_eq!(app.target_scope.as_ref(), Some(&root));
        let requests = app.drain_requests();
        assert!(matches!(&requests[0], Request::Plan { request, .. } if request.path == root));
        app.on_worker(WorkerEvent::Planned {
            scope: root.clone(),
            generation: app.targets().unwrap().generation,
            purpose: PlanPurpose::Detect,
            result: Ok(plan.clone()),
        });
        let highlighted = app.targets().unwrap().highlighted.clone().unwrap();
        assert_eq!(
            plan.targets
                .iter()
                .find(|r| r.candidate_key == highlighted)
                .unwrap()
                .target
                .path,
            Some(root.join(name))
        );
        app.handle(Action::Back);
        app.handle(Action::Targets);
        assert!(app.drain_requests().is_empty());
        assert_eq!(
            app.targets().unwrap().highlighted.as_ref(),
            Some(&highlighted)
        );
    }
}

#[test]
fn review_drops_vanished_replaced_and_newly_refused_ticks_without_widening() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    std::fs::write(root.join("package.json"), b"{}").unwrap();
    for name in [".next", "dist", "node_modules"] {
        std::fs::create_dir(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("payload"), vec![1; 4096]).unwrap();
    }
    let baseline = Arc::new(
        pipeline::build_reviewed_plan(
            worker::plan_request(
                root.clone(),
                Some(root.join("home")),
                CleanupLevel::Safe,
                false,
                None,
            ),
            Arc::new(Quiet),
        )
        .unwrap(),
    );
    let mut app = App::new(root.clone());
    app.drain_requests();
    app.handle(Action::Targets);
    app.drain_requests();
    app.on_worker(WorkerEvent::Planned {
        scope: root.clone(),
        generation: app.targets().unwrap().generation,
        purpose: PlanPurpose::Detect,
        result: Ok(baseline.clone()),
    });
    app.handle(Action::Invert);
    // Keep the old inode alive to prevent inode reuse in the replacement test.
    std::fs::rename(root.join("dist"), root.join("old-dist")).unwrap();
    std::fs::create_dir(root.join("dist")).unwrap();
    std::fs::remove_dir_all(root.join(".next")).unwrap();
    std::fs::create_dir_all(root.join("new-project/.next")).unwrap();
    std::fs::write(root.join("new-project/package.json"), b"{}").unwrap();
    app.handle(Action::Review);
    assert!(
        rendered(&app, false, 80, 24)
            .0
            .contains("building reviewed plan…")
    );
    let mut requests = app.drain_requests();
    let Request::Plan {
        request,
        generation,
        ..
    } = requests.pop().unwrap()
    else {
        panic!("expected review");
    };
    let plan =
        Arc::new(pipeline::build_tui_reviewed_plan(*request, Arc::new(Quiet), &baseline).unwrap());
    assert_eq!(plan.actions.len(), 1);
    assert_eq!(plan.actions[0].target.path, Some(root.join("node_modules")));
    app.on_worker(WorkerEvent::Planned {
        scope: root.clone(),
        generation,
        purpose: PlanPurpose::Review,
        result: Ok(plan),
    });
    assert_eq!(app.screen(), Screen::Review);
    let lines = views::review::lines(&app, Theme { colors: false });
    assert!(
        lines[0]
            .to_string()
            .contains("2 selected items changed since detection and were dropped")
    );
    assert!(lines[0].to_string().contains("dist"));
    assert!(lines[0].to_string().contains(".next"));
    assert!(
        rendered(&app, true, 80, 24)
            .1
            .content
            .iter()
            .any(|c| c.fg == Color::Yellow)
    );
    // A freshly detected symlinked configured spelling resolves to the same
    // stable key, but is refused. The tick must disappear even though the
    // target itself still exists with the same inode and operation.
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".config/cleanrs")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&root, root.join("linked")).unwrap();
    std::fs::write(home.join(".config/cleanrs/targets.toml"), format!(
        "[[target]]\nid = \"node.modules\"\ndetect = {{ path = {:?} }}\nclass = \"network\"\nreclaim = {{ method = \"delete\" }}\n",
        root.join("linked/node_modules").display().to_string()
    )).unwrap();
    let keys = baseline
        .targets
        .iter()
        .map(|r| r.candidate_key.clone())
        .collect();
    let plan = pipeline::build_tui_reviewed_plan(
        worker::plan_request(
            root.clone(),
            Some(home),
            CleanupLevel::Safe,
            false,
            Some(keys),
        ),
        Arc::new(Quiet),
        &baseline,
    )
    .unwrap();
    assert!(plan.actions.is_empty());
    assert!(
        plan.targets
            .iter()
            .any(|r| matches!(r.target.safety, CandidateSafety::Refused(_)))
    );
}

#[test]
fn busy_indicators_animate_without_worker_progress() {
    let (_fixture, mut app) = fixture();
    let render_lines = |app: &App| {
        rendered(app, false, 100, 24)
            .1
            .content
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let scope = app.target_scope.clone().unwrap();
    for reviewing in [false, true] {
        let targets = app.targets_cache.get_mut(&scope).unwrap();
        targets.planning = true;
        targets.reviewing = reviewing;
        targets.activity = "inspecting system provider: pip…".into();
        assert!(app.busy());
        // Advance only the timer: a quiet provider must not freeze its spinner.
        let before = render_lines(&app);
        app.on_tick();
        let after = render_lines(&app);
        let before = before.lines().nth(1).unwrap();
        let after = after.lines().nth(1).unwrap();
        assert_ne!(before.chars().next(), after.chars().next());
        assert_eq!(
            before.chars().skip(1).collect::<String>(),
            after.chars().skip(1).collect::<String>()
        );
        assert!(after.contains("inspecting system provider: pip"));
    }
    app.targets_cache.get_mut(&scope).unwrap().planning = false;
    run_fixture(&mut app);
    app.run.as_mut().unwrap().actions[0] = RunAction::Running;
    assert!(app.busy());
    let before = render_lines(&app);
    app.on_tick();
    let after = render_lines(&app);
    assert_ne!(before.lines().nth(1), after.lines().nth(1));
    let running_row = |text: &str| {
        text.lines()
            .find(|line| line.contains("running"))
            .unwrap()
            .to_owned()
    };
    assert_ne!(running_row(&before), running_row(&after));
    assert!(after.contains("action 1/1 · 0 completed"));
}

#[test]
fn freshness_is_human_and_stale_views_warn_with_refresh_hint() {
    assert_eq!(views::freshness(Duration::from_secs(59)), "59s ago");
    assert_eq!(views::freshness(Duration::from_secs(60)), "1m ago");
    assert_eq!(views::freshness(Duration::from_secs(600)), "10m ago");
    assert_eq!(
        views::freshness(Duration::from_secs(3600)),
        "1h ago · press r to refresh"
    );
    let (_fixture, mut app) = fixture();
    let finished = std::time::Instant::now() - Duration::from_secs(601);
    app.targets_cache
        .get_mut(app.target_scope.as_ref().unwrap())
        .unwrap()
        .finished = finished;
    app.directories.get_mut(&app.current).unwrap().finished = finished;
    app.directories.get_mut(&app.current).unwrap().listed = true;
    for screen in [Screen::Targets, Screen::Browser] {
        app.screens = vec![screen];
        let (text, buffer) = rendered(&app, true, 80, 24);
        assert!(text.contains("10m ago · press r to refresh"));
        assert!(buffer.content.iter().any(|c| c.fg == Color::Yellow));
        assert!(
            rendered(&app, false, 80, 24)
                .0
                .contains("press r to refresh")
        );
    }
}

#[test]
fn invalid_scope_planning_error_is_red_dismissable_and_navigation_continues() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("missing");
    let mut app = App::new(root.clone());
    app.home = Some(fixture.path().into());
    app.drain_requests();
    app.handle(Action::Targets);
    let workers = Workers::start();
    for request in app.drain_requests() {
        workers.requests.send(request).unwrap();
    }
    loop {
        let event = workers
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        let complete = matches!(event, WorkerEvent::Planned { .. });
        app.on_worker(event);
        if complete {
            break;
        }
    }
    assert!(app.status.as_ref().unwrap().1);
    let (text, buffer) = rendered(&app, true, 80, 24);
    assert!(text.contains("✗ failed to build cleanup plan"));
    assert!(buffer.content.iter().any(|c| c.fg == Color::Red));
    app.handle(Action::Move(1));
    assert!(app.status.is_none());
    app.handle(Action::Back);
    assert_eq!(app.screen(), Screen::Browser);
    app.on_worker(WorkerEvent::Coverage {
        parent: root,
        revision: 0,
    });
    assert!(app.status.is_none());
    assert!(
        rendered(&app, false, 80, 24)
            .0
            .contains("⚠ incomplete coverage")
    );
}

#[test]
fn every_screen_help_footer_and_small_terminal_guard_work_without_color() {
    let (_fixture, mut app) = fixture();
    let mut plan = fabricated(&app.current);
    plan.selected_estimate.explanation = None;
    for row in &mut plan.targets {
        if let UsageEstimate::Filesystem { reclaimable, .. } = &mut row.target.usage {
            reclaimable.explanation = None;
        }
    }
    let target = app
        .targets_cache
        .get_mut(app.target_scope.as_ref().unwrap())
        .unwrap();
    target.baseline = Some(Arc::new(plan));
    assert!(
        !rendered(&app, false, 80, 24)
            .0
            .contains("confidence Medium ·")
    );
    review(&mut app);
    run_fixture(&mut app);
    for screen in [
        Screen::Browser,
        Screen::Targets,
        Screen::Review,
        Screen::Run,
        Screen::Results,
    ] {
        app.screens = vec![screen];
        let (text, _) = rendered(&app, false, 80, 24);
        assert!(!text.contains("terminal too small"));
        let footer = super::keymap::footer_for(screen, 80);
        assert!(ratatui::text::Line::raw(&footer).width() <= 80);
        assert!(footer.contains("? help"));
        assert!(footer.contains(if screen == Screen::Browser {
            "q quit"
        } else if screen == Screen::Run {
            "q stop"
        } else {
            "q back"
        }));
        for size in [(60, 15), (79, 24), (80, 19)] {
            assert!(
                rendered(&app, false, size.0, size.1)
                    .0
                    .contains("terminal too small (need 80x20)")
            );
        }
        app.handle(Action::Help);
        let bindings: Vec<_> = super::keymap::bindings(screen).collect();
        for (i, binding) in bindings.iter().enumerate() {
            app.help_scroll = i;
            let help = rendered(&app, false, 80, 24).0;
            assert!(
                help.contains(binding.label),
                "missing {:?} help {}",
                screen,
                binding.label
            );
            assert!(
                help.contains(super::keymap::help_for(binding, screen)),
                "clipped help for {:?} {}",
                screen,
                binding.label
            );
        }
    }
}

#[test]
fn review_drops_grown_allocation_instead_of_silently_widening_cached_selection() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    std::fs::write(root.join("package.json"), b"{}").unwrap();
    for name in [".next", "dist"] {
        std::fs::create_dir(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("payload"), vec![1; 4096]).unwrap();
    }
    let baseline = pipeline::build_reviewed_plan(
        worker::plan_request(
            root.clone(),
            Some(root.join("home")),
            CleanupLevel::Safe,
            false,
            None,
        ),
        Arc::new(Quiet),
    )
    .unwrap();
    let keys = baseline
        .targets
        .iter()
        .map(|r| r.candidate_key.clone())
        .collect();
    std::fs::write(root.join(".next/payload"), vec![1; 8192]).unwrap();
    let reviewed = pipeline::build_tui_reviewed_plan(
        worker::plan_request(
            root.clone(),
            Some(root.join("home")),
            CleanupLevel::Safe,
            false,
            Some(keys),
        ),
        Arc::new(Quiet),
        &baseline,
    )
    .unwrap();
    assert_eq!(reviewed.actions.len(), 1);
    assert_eq!(reviewed.actions[0].target.path, Some(root.join("dist")));
}

#[test]
fn cached_summary_keeps_provider_reported_and_unknown_estimates_separate() {
    let root = Path::new("/fixture");
    let mut targets: Vec<_> = ["a", "b", "c", "d"]
        .into_iter()
        .map(|id| candidate(root, id, TargetClass::Free))
        .collect();
    targets[0].usage = UsageEstimate::ProviderReported {
        bytes: Some(2048),
        confidence: Confidence::Medium,
        explanation: "native estimate".into(),
    };
    targets[1].usage = UsageEstimate::ProviderReported {
        bytes: Some(4096),
        confidence: Confidence::High,
        explanation: "native estimate".into(),
    };
    targets[2].usage = UsageEstimate::ProviderReported {
        bytes: None,
        confidence: Confidence::Low,
        explanation: "no native estimate".into(),
    };
    targets[3].usage = UsageEstimate::Unknown {
        explanation: "unknown storage".into(),
    };
    let planned = plan_cleanup(targets, CleanupLevel::Safe, &[], false);
    let accounting =
        pipeline::SelectionAccounting::new(cleanrs_core::fs::Arena::new(root), true, &planned);
    let summary = accounting.summarize(planned.iter());
    assert_eq!(summary.count, 4);
    assert_eq!(summary.provider_reported, 6144);
    assert_eq!(summary.provider_actions, 3);
    assert_eq!(summary.unknown_actions, 1);
    assert_eq!(summary.estimate.ceiling, Some(0));
}

#[test]
fn review_walk_is_limited_to_selected_trees() {
    use std::{sync::Mutex, time::Instant};
    struct Counting(Mutex<u64>);
    impl PlanObserver for Counting {
        fn on_event(&self, _: PlanEvent) {}
        fn on_scan_progress(&self, progress: ScanProgress) {
            *self.0.lock().unwrap() = progress.directories;
        }
    }
    let fixture = tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("chosen/.next")).unwrap();
    std::fs::write(root.join("chosen/package.json"), b"{}").unwrap();
    std::fs::write(root.join("chosen/.next/payload"), vec![0; 4096]).unwrap();
    for n in 0..100 {
        std::fs::create_dir_all(root.join(format!("unrelated/{n}/deep"))).unwrap();
        std::fs::write(
            root.join(format!("unrelated/{n}/deep/payload")),
            b"unrelated",
        )
        .unwrap();
    }
    let started = Instant::now();
    let baseline = pipeline::build_reviewed_plan(
        worker::plan_request(
            root.clone(),
            Some(root.join("home")),
            CleanupLevel::Safe,
            false,
            None,
        ),
        Arc::new(Quiet),
    )
    .unwrap();
    let full_elapsed = started.elapsed();
    let key = baseline
        .targets
        .iter()
        .find(|t| t.target.id == "node.next")
        .unwrap()
        .candidate_key
        .clone();
    let observer = Arc::new(Counting(Mutex::new(0)));
    let started = Instant::now();
    let reviewed = pipeline::build_tui_reviewed_plan(
        worker::plan_request(
            root.clone(),
            Some(root.join("home")),
            CleanupLevel::Safe,
            false,
            Some(HashSet::from([key])),
        ),
        observer.clone(),
        &baseline,
    )
    .unwrap();
    eprintln!(
        "fixture review: full discovery {:?}; selected review {:?}",
        full_elapsed,
        started.elapsed()
    );
    assert_eq!(reviewed.actions.len(), 1);
    assert_eq!(
        reviewed.actions[0].target.path,
        Some(root.join("chosen/.next"))
    );
    assert_eq!(
        *observer.0.lock().unwrap(),
        1,
        "unrelated directories must never be walked"
    );
}

#[test]
fn relative_project_paths_and_wrapped_review_end_are_visible() {
    let (_dir, mut app) = fixture();
    let scope = app.target_scope.clone().unwrap();
    let targets = app.targets_cache.get_mut(&scope).unwrap();
    let baseline = Arc::make_mut(targets.baseline.as_mut().unwrap());
    baseline.targets[0].target.project_root = Some(scope.join("first/apps/web"));
    baseline.targets[1].target.project_root = Some(scope.join("second/apps/web"));
    let text = rendered(&app, false, 150, 32).0;
    assert!(text.contains("Project path"));
    assert!(text.contains("first/apps/web") && text.contains("second/apps/web"));
    app.handle(Action::Invert);
    review(&mut app);
    app.page_width = 80;
    app.page_rows = 13;
    app.handle(Action::Last);
    let bottom = rendered(&app, false, 80, 24).0;
    assert!(bottom.contains("After cleanup: regenerate with fixture build"));
    let end = app.review_scroll;
    app.handle(Action::Move(-1));
    assert_eq!(app.review_scroll, end.saturating_sub(1));
    app.handle(Action::Cancel);
    assert!(app.reviewed.is_none());
    app.handle(Action::Rescan);
    assert!(
        matches!(
            app.drain_requests().last(),
            Some(Request::Plan {
                browser_scan: None,
                ..
            })
        ),
        "explicit refresh must bypass the browser cache"
    );
}

#[test]
fn quit_dialog_is_visible_and_escape_enter_keep_the_browser() {
    for colors in [false, true] {
        let (_dir, mut app) = fixture();
        app.screens.truncate(1);
        app.drain_requests();
        let mut map = Keymap::default();
        press(&mut app, &mut map, KeyCode::Esc, KeyModifiers::NONE);
        let text = rendered(&app, colors, 80, 24).0;
        assert!(text.contains("Quit cleanrs?") && text.contains("[n / Esc / Enter] Stay"));
        press(&mut app, &mut map, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.quit && !app.confirm_quit);
        press(&mut app, &mut map, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut app, &mut map, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.quit && !app.confirm_quit);
        assert!(app.drain_requests().is_empty());
        press(&mut app, &mut map, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut app, &mut map, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(app.quit);
    }
}

#[test]
fn browser_cache_survives_restart_parent_navigation_and_explicit_refresh() {
    use super::cache::DiskCache;
    use std::time::Instant;
    fn finish(app: &mut App, workers: &Workers) {
        for request in app.drain_requests() {
            workers.requests.send(request).unwrap();
        }
        while app.directory().busy() {
            let event = workers
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            app.on_worker(event);
        }
    }
    let fixture = tempdir().unwrap();
    let workspace = fixture.path().join("workspace");
    let project = workspace.join("project");
    std::fs::create_dir_all(project.join(".next/deep")).unwrap();
    std::fs::create_dir(project.join("dist")).unwrap();
    std::fs::write(project.join("package.json"), b"{}").unwrap();
    let payload = project.join(".next/deep/payload");
    std::fs::write(&payload, vec![7; 8192]).unwrap();
    std::fs::hard_link(&payload, project.join("dist/shared")).unwrap();
    let project = project.canonicalize().unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let cache = DiskCache::new(fixture.path().join("cache"));
    let started = Instant::now();
    let workers = Workers::start_with_cache(Some(cache.clone()));
    let mut app = App::new(project.clone());
    finish(&mut app, &workers);
    let initial = started.elapsed();
    let original_total = app.directory().total;
    assert!(!app.directory().cached);
    drop(workers);
    // A new worker/App shares no in-memory arenas with the first session.
    let started = Instant::now();
    let workers = Workers::start_with_cache(Some(cache.clone()));
    let mut reopened = App::new(project.clone());
    finish(&mut reopened, &workers);
    assert!(reopened.directory().cached);
    assert_eq!(reopened.directory().total, original_total);
    eprintln!(
        "browser fixture: fresh {:?}; restart {:?}",
        initial,
        started.elapsed()
    );
    reopened.handle(Action::Parent);
    finish(&mut reopened, &workers);
    assert_eq!(reopened.current, workspace);
    assert!(
        reopened.directory().cached,
        "previous browser root is reconstructed from its per-child cache entries"
    );
    assert_eq!(reopened.directory().total, original_total);
    reopened.handle(Action::Open);
    assert_eq!(reopened.current, project);
    assert!(reopened.drain_requests().is_empty());
    reopened.handle(Action::Parent);
    assert!(
        reopened.drain_requests().is_empty(),
        "visited parent must never rescan"
    );
    std::fs::write(&payload, vec![7; 32768]).unwrap();
    reopened.handle(Action::Rescan);
    finish(&mut reopened, &workers);
    assert!(!reopened.directory().cached);
    assert!(reopened.directory().total > original_total);
    assert!(reopened.status.is_none());
    drop(workers);
    // A corrupt cache entry must fall back to a fresh scan, preserving use of
    // the browser rather than treating cached sizes as authoritative facts.
    for entry in std::fs::read_dir(fixture.path().join("cache")).unwrap() {
        std::fs::write(entry.unwrap().path(), b"corrupt").unwrap();
    }
    let workers = Workers::start_with_cache(Some(cache));
    let mut recovered = App::new(project);
    finish(&mut recovered, &workers);
    assert!(!recovered.directory().cached);
    assert!(recovered.directory().total > original_total);
}

#[test]
fn cache_write_failure_keeps_sizes_and_reports_session_only_fallback() {
    use super::cache::DiskCache;
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    std::fs::create_dir_all(root.join("child")).unwrap();
    std::fs::write(root.join("child/payload"), vec![1; 8192]).unwrap();
    let blocked = fixture.path().join("not-a-directory");
    std::fs::write(&blocked, b"fixture").unwrap();
    let workers = Workers::start_with_cache(Some(DiskCache::new(blocked)));
    let mut app = App::new(root.canonicalize().unwrap());
    for request in app.drain_requests() {
        workers.requests.send(request).unwrap();
    }
    while app.directory().busy() {
        app.on_worker(
            workers
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap(),
        );
    }
    assert!(app.directory().total > 0);
    let (message, error) = app.status.as_ref().unwrap();
    assert!(
        !error
            && message.contains("Disk cache unavailable")
            && message.contains("sizes kept for this session")
    );
}

#[test]
fn git_project_selection_review_checks_tracked_paths_without_reading_unselected_source() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
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
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    std::fs::write(root.join("package.json"), "{}").unwrap();
    std::fs::write(root.join(".gitignore"), ".next/\n").unwrap();
    git(&["init", "-q", "--initial-branch=main"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "fixture"]);
    std::fs::create_dir(root.join(".next")).unwrap();
    std::fs::write(root.join(".next/output"), "generated output").unwrap();
    let baseline = pipeline::build_reviewed_plan(
        worker::plan_request(root.clone(), None, CleanupLevel::Safe, false, None),
        Arc::new(Quiet),
    )
    .unwrap();
    let key = baseline
        .targets
        .iter()
        .find(|p| p.target.id == "node.next")
        .unwrap()
        .candidate_key
        .clone();
    let request = || {
        worker::plan_request(
            root.clone(),
            None,
            CleanupLevel::Safe,
            false,
            Some(HashSet::from([key.clone()])),
        )
    };
    let review = pipeline::build_tui_reviewed_plan(request(), Arc::new(Quiet), &baseline).unwrap();
    assert_eq!(review.actions.len(), 1);
    let evidence = review.projects.projects[0].git.as_ref().unwrap();
    assert!(evidence.complete && !evidence.activity_inspected);
    assert_eq!(review.projects.inspection_commands, 2);
    git(&["add", "--force", ".next/output"]);
    let changed = pipeline::build_tui_reviewed_plan(request(), Arc::new(Quiet), &baseline).unwrap();
    assert!(
        changed.actions.is_empty(),
        "new tracked source must drop the old tick"
    );
    assert!(root.join(".next/output").exists());
}

#[test]
fn system_shortcut_is_global_cached_separate_and_returns_to_its_origin() {
    let (_dir, mut app) = fixture();
    let folder = app.target_scope.clone();
    let selected = app.targets().unwrap().selected.clone();
    app.drain_requests();
    let mut keys = Keymap::default();
    press(&mut app, &mut keys, KeyCode::Char('S'), KeyModifiers::NONE);
    assert!(app.targets().unwrap().system);
    let requests = app.drain_requests();
    let Request::Plan {
        request,
        browser_scan,
        ..
    } = requests.last().unwrap()
    else {
        panic!("system detection");
    };
    assert!(request.system && !request.workspace_only);
    assert!(browser_scan.is_none());
    let generation = app.targets().unwrap().generation;
    let mut system_plan = fabricated(_dir.path());
    system_plan.system = true;
    system_plan.scope = crate::pipeline::system_scope();
    app.on_worker(WorkerEvent::Planned {
        scope: crate::pipeline::system_scope(),
        generation,
        purpose: PlanPurpose::Detect,
        result: Ok(Arc::new(system_plan)),
    });
    assert!(rendered(&app, false, 120, 30).0.contains("system"));
    app.handle(Action::Toggle);
    let system_selection = app.targets().unwrap().selected.clone();
    app.handle(Action::Back);
    assert_eq!(app.target_scope, folder);
    assert_eq!(app.targets().unwrap().selected, selected);
    press(&mut app, &mut keys, KeyCode::Char('S'), KeyModifiers::NONE);
    assert!(
        app.drain_requests().is_empty(),
        "system cache reopens without detection"
    );
    assert_eq!(app.targets().unwrap().selected, system_selection);
    app.handle(Action::Rescan);
    assert!(
        matches!(app.drain_requests().last(), Some(Request::Plan { request, .. }) if request.system)
    );
    app.handle(Action::Back);
    app.handle(Action::Back);
    assert_eq!(app.screen(), Screen::Browser);
    press(&mut app, &mut keys, KeyCode::Char('S'), KeyModifiers::NONE);
    assert_eq!(app.screen(), Screen::Targets);
    app.handle(Action::Back);
    assert_eq!(app.screen(), Screen::Browser);
    assert!(
        keys.action(
            KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE),
            Screen::Review,
            false
        )
        .is_none()
    );
    assert!(
        keys.action(
            KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE),
            Screen::Run,
            false
        )
        .is_none()
    );
    assert!(
        keys.action(
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
            Screen::Targets,
            false
        )
        .is_none()
    );
}

#[test]
fn system_review_results_restore_prior_folder_results() {
    let (dir, mut app) = fixture();
    run_fixture(&mut app);
    app.screens.pop();
    app.screens.push(Screen::Results);
    let folder_scope = app.run.as_ref().unwrap().plan.scope.clone();
    app.handle(Action::SystemTargets);
    let scope = crate::pipeline::system_scope();
    let generation = app.targets().unwrap().generation;
    let mut plan = fabricated(dir.path());
    plan.system = true;
    plan.scope = scope.clone();
    app.on_worker(WorkerEvent::Planned {
        scope,
        generation,
        purpose: PlanPurpose::Detect,
        result: Ok(Arc::new(plan)),
    });
    review(&mut app);
    assert_eq!(app.screen(), Screen::Review);
    assert!(app.reviewed.as_ref().unwrap().system);
    app.handle(Action::Back);
    run_fixture(&mut app);
    app.screens.pop();
    app.screens.push(Screen::Results);
    app.handle(Action::Back);
    assert_eq!(app.screen(), Screen::Results);
    assert_eq!(app.run.as_ref().unwrap().plan.scope, folder_scope);
    assert!(!app.run.as_ref().unwrap().plan.system);
}
