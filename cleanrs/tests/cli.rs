use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use tempfile::tempdir;

#[test]
fn scan_json_is_machine_readable_and_defaults_to_allocated() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("payload"), b"cleanrs fixture").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "scan",
            fixture.path().to_str().unwrap(),
            "--json",
            "--depth",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        output.stderr.is_empty(),
        "JSON mode must not emit progress output"
    );
    assert_eq!(json["metric"], "allocated");
    assert_eq!(json["coverage_complete"], true);
    assert!(json["entries"][0]["allocated"].as_u64().unwrap() > 0);
}

#[test]
fn human_scan_output_has_table_headers_without_ansi_when_piped() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("payload"), b"cleanrs fixture").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["scan", fixture.path().to_str().unwrap(), "--depth", "0"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("ALLOCATED"));
    assert!(stdout.contains("SCAN SUMMARY"));
    assert!(!stdout.contains("\u{1b}["));
}

#[test]
fn doctor_output_is_grouped_into_human_sections() {
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .arg("doctor")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("CONTAINER"));
    assert!(stdout.contains("Volumes"));
    assert!(stdout.contains("Data attribution"));
    assert!(stdout.contains("Diagnostics"));
}

#[test]
fn mutually_exclusive_metrics_are_rejected() {
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["scan", ".", "--allocated", "--apparent"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn symlink_scan_root_is_rejected() {
    #[cfg(unix)]
    {
        let fixture = tempdir().unwrap();
        let target = fixture.path().join("target");
        let link = fixture.path().join("link");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
            .args(["scan", link.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("symbolic link"));
    }
}

#[test]
fn targets_detects_and_sizes_workspace_artifacts_as_json() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("package.json"), b"{}").unwrap();
    fs::create_dir(fixture.path().join("node_modules")).unwrap();
    fs::write(
        fixture.path().join("node_modules").join("package.js"),
        b"module.exports = {};",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--json",
            "--only",
            "node.modules",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["targets"][0]["id"], "node.modules");
    assert_eq!(json["targets"][0]["safety"]["status"], "eligible");
    assert!(json["targets"][0]["usage"]["allocated"].as_u64().unwrap() > 0);
    assert!(
        json["targets"][0]["usage"]["reclaimable"]["ceiling"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(json["targets"][0]["operation"]["method"], "direct_delete");
}

#[test]
fn targets_human_output_is_explicitly_read_only() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("Cargo.toml"), b"[package]").unwrap();
    fs::create_dir(fixture.path().join("target")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--workspace-only",
        ])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("RECLAIMABLE"));
    assert!(stdout.contains("PROJECT"));
    assert!(stdout.contains("LOCATION"));
    assert!(stdout.contains("cargo.target"));
    assert!(stdout.contains("read-only; no action was run"));
    assert!(!stdout.contains("LOCATION / OPERATION"));
    assert!(!stdout.contains("\u{1b}["));
}

#[test]
fn clean_human_output_prioritizes_project_and_location() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("package.json"), b"{}").unwrap();
    fs::create_dir(fixture.path().join("node_modules")).unwrap();
    fs::write(fixture.path().join("node_modules/payload"), b"generated").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--workspace-only",
            "--level",
            "standard",
            "--ignore-staleness",
        ])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let header = stdout.lines().find(|line| line.contains("ACTION")).unwrap();
    assert!(header.contains("RECLAIMABLE"));
    assert!(header.contains("PROJECT"));
    assert!(header.contains("LOCATION"));
    assert!(!header.contains("OPERATION"));
    assert!(!stdout.contains("delete "));
}

#[test]
fn targets_never_loads_working_directory_configuration() {
    let fixture = tempdir().unwrap();
    fs::write(
        fixture.path().join("cleanrs.toml"),
        r#"[[target]]
id = "hostile"
detect = { command = "sh" }
reclaim = { method = "run", argv = ["sh", "-c", "touch owned"] }
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .current_dir(fixture.path())
        .args(["targets", ".", "--json", "--workspace-only"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["targets"].as_array().unwrap().len(), 0);
    assert!(!fixture.path().join("owned").exists());
}

#[test]
fn targets_rejects_unknown_filter_ids() {
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["targets", ".", "--only", "does.not.exist"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown target ID"));
}

#[test]
fn targets_collapses_package_artifacts_inside_node_modules() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("package.json"), b"{}").unwrap();
    let dependency = fixture.path().join("node_modules/dependency");
    fs::create_dir_all(dependency.join("dist")).unwrap();
    fs::write(dependency.join("package.json"), b"{}").unwrap();
    fs::write(dependency.join("dist/bundle.js"), b"generated").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--json",
            "--workspace-only",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let targets = json["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0]["id"], "node.modules");
}

#[test]
fn targets_top_returns_the_largest_candidates_first() {
    let fixture = tempdir().unwrap();
    for (project, bytes) in [("small", 1_usize), ("large", 1024 * 1024)] {
        let root = fixture.path().join(project);
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::write(root.join("package.json"), b"{}").unwrap();
        fs::write(root.join("node_modules/payload"), vec![0_u8; bytes]).unwrap();
    }

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--json",
            "--top",
            "1",
            "--workspace-only",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["total_detected"], 2);
    assert_eq!(json["hidden"], 1);
    assert!(
        json["targets"][0]["path"]
            .as_str()
            .unwrap()
            .contains("large/node_modules")
    );
}

#[test]
fn clean_dry_run_selects_without_deleting() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("package.json"), b"{}").unwrap();
    fs::create_dir(fixture.path().join("node_modules")).unwrap();
    let payload = fixture.path().join("node_modules/payload");
    fs::write(&payload, b"must survive dry-run").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--level",
            "standard",
            "--ignore-staleness",
            "--only",
            "node.modules",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["execution"], "dry-run; no action was run");
    assert_eq!(json["selected_targets"], 1);
    assert_eq!(json["targets"][0]["selection"]["status"], "selected");
    assert!(
        payload.exists(),
        "dry-run must leave every candidate untouched"
    );
}

#[test]
fn clean_safe_level_explains_why_network_targets_are_skipped() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("package.json"), b"{}").unwrap();
    fs::create_dir(fixture.path().join("node_modules")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--ignore-staleness",
            "--workspace-only",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["selected_targets"], 0);
    assert!(
        json["targets"][0]["selection"]["reason"]
            .as_str()
            .unwrap()
            .contains("cleanup level")
    );
}

#[test]
fn clean_apply_is_inert_even_for_a_valid_fixture() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("package.json"), b"{}").unwrap();
    fs::create_dir(fixture.path().join("node_modules")).unwrap();
    let payload = fixture.path().join("node_modules/payload");
    fs::write(&payload, b"must survive apply refusal").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--apply",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not available yet"));
    assert!(
        payload.exists(),
        "inert --apply must not alter its candidate"
    );
}

#[cfg(unix)]
fn install_fake_brew(root: &std::path::Path) -> std::path::PathBuf {
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let brew = bin.join("brew");
    fs::write(
        &brew,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "Homebrew 6.0.18"
elif [ "$1" = "--prefix" ]; then
  echo "/fixture/homebrew"
elif [ "$1" = "--cache" ]; then
  echo "/fixture/cache"
elif [ "$1" = "cleanup" ] && [ "$2" = "--dry-run" ] && [ "$3" = "--scrub" ]; then
  echo "Would remove: /fixture/cache/a (2GB)"
  echo "This operation would free approximately 2GB of disk space."
elif [ "$1" = "cleanup" ] && [ "$2" = "--dry-run" ]; then
  echo "Would remove: /fixture/cache/a (1.2GB)"
  echo "This operation would free approximately 1.2GB of disk space."
elif [ "$1" = "autoremove" ] && [ "$2" = "--dry-run" ]; then
  echo "Would autoremove 2 unneeded formulae:"
  echo "example-a"
  echo "example-b"
else
  exit 2
fi
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&brew).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&brew, permissions).unwrap();
    bin
}

#[test]
#[cfg(unix)]
fn targets_inspects_homebrew_and_emits_exact_command_plan() {
    let fixture = tempdir().unwrap();
    let bin = install_fake_brew(fixture.path());
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--only",
            "homebrew.cleanup",
            "--json",
        ])
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["targets"].as_array().unwrap().len(), 1);
    let target = &json["targets"][0];
    assert_eq!(target["id"], "homebrew.cleanup");
    assert_eq!(target["provider"]["id"], "homebrew");
    assert_eq!(target["usage"]["source"], "provider_reported");
    assert_eq!(target["usage"]["bytes"], 1_200_000_000_u64);
    assert_eq!(target["operation"]["method"], "command");
    assert_eq!(target["operation"]["command"]["args"][0], "cleanup");
    assert_eq!(
        target["operation"]["command"]["environment"]["HOMEBREW_NO_AUTOREMOVE"],
        "1"
    );
    assert!(
        target["consequence"]
            .as_str()
            .unwrap()
            .contains("downloads")
    );
    assert_eq!(json["providers"][0]["detected"], true);
}

#[test]
#[cfg(unix)]
fn clean_deduplicates_overlapping_homebrew_cleanup_modes() {
    let fixture = tempdir().unwrap();
    let bin = install_fake_brew(fixture.path());
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--level",
            "aggressive",
            "--only",
            "homebrew.cleanup",
            "--only",
            "homebrew.cleanup.scrub",
            "--json",
        ])
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["execution"], "dry-run; no action was run");
    assert_eq!(json["selected_targets"], 1);
    assert_eq!(json["selected_provider_reported"], 2_000_000_000_u64);
    let targets = json["targets"].as_array().unwrap();
    let routine = targets
        .iter()
        .find(|target| target["id"] == "homebrew.cleanup")
        .unwrap();
    assert!(
        routine["selection"]["reason"]
            .as_str()
            .unwrap()
            .contains("superseded")
    );
    let scrub = targets
        .iter()
        .find(|target| target["id"] == "homebrew.cleanup.scrub")
        .unwrap();
    assert_eq!(scrub["selection"]["status"], "selected");
}

#[test]
fn explicit_config_adds_a_scoped_path_target() {
    let fixture = tempdir().unwrap();
    let artifact = fixture.path().join("generated");
    fs::create_dir(&artifact).unwrap();
    fs::write(artifact.join("payload"), b"generated").unwrap();
    let config = fixture.path().join("targets.toml");
    fs::write(
        &config,
        format!(
            r#"[[target]]
id = "custom.generated"
description = "fixture generated output"
detect = {{ path = {:?} }}
class = "free"
reclaim = {{ method = "delete" }}
stale_after = "0d"
"#,
            artifact.display().to_string()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--no-builtin-targets",
            "--json",
        ])
        .env("HOME", fixture.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["targets"][0]["id"], "custom.generated");
    assert_eq!(json["targets"][0]["operation"]["method"], "direct_delete");
    assert!(
        artifact.exists(),
        "registry inspection must remain read-only"
    );
}

#[test]
fn configured_path_normalization_cannot_widen_to_the_scan_root() {
    let fixture = tempdir().unwrap();
    let physical_root = fixture.path().canonicalize().unwrap();
    let nested = physical_root.join("project/generated");
    fs::create_dir_all(&nested).unwrap();
    let config = fixture.path().join("targets.toml");
    fs::write(
        &config,
        format!(
            r#"[[target]]
id = "custom.widened"
detect = {{ path = {:?} }}
class = "free"
reclaim = {{ method = "delete" }}
"#,
            nested.join("../..").display().to_string()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            physical_root.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--no-builtin-targets",
            "--json",
        ])
        .env("HOME", &physical_root)
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let reason = json["targets"][0]["safety"]["reason"].as_str().unwrap();
    assert!(reason.contains("scan root"), "unexpected refusal: {reason}");
}

#[test]
fn explicit_config_command_requires_a_per_run_trust_decision() {
    let fixture = tempdir().unwrap();
    let config = fixture.path().join("targets.toml");
    fs::write(
        &config,
        r#"[[target]]
id = "custom.command"
detect = { command = "echo" }
class = "free"
reclaim = { method = "run", argv = ["echo", "never executed"] }
"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--no-builtin-targets",
            "--json",
        ])
        .env("HOME", fixture.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--trust-config-commands"));
}

#[test]
fn config_rejects_unknown_fields_and_detector_operation_mismatches() {
    let fixture = tempdir().unwrap();
    let unknown = fixture.path().join("unknown.toml");
    fs::write(
        &unknown,
        r#"[[target]]
id = "custom.typo"
detect = { path = ".", exsits = true }
class = "free"
reclaim = { method = "delete" }
"#,
    )
    .unwrap();
    let unknown_output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--config",
            unknown.to_str().unwrap(),
            "--no-builtin-targets",
        ])
        .env("HOME", fixture.path())
        .output()
        .unwrap();
    assert!(!unknown_output.status.success());
    assert!(String::from_utf8_lossy(&unknown_output.stderr).contains("unknown field"));

    let mismatch = fixture.path().join("mismatch.toml");
    fs::write(
        &mismatch,
        r#"[[target]]
id = "custom.mismatch"
detect = { command = "echo" }
class = "free"
reclaim = { method = "run", argv = ["printf", "never executed"] }
"#,
    )
    .unwrap();
    let mismatch_output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--config",
            mismatch.to_str().unwrap(),
            "--trust-config-commands",
            "--no-builtin-targets",
        ])
        .env("HOME", fixture.path())
        .output()
        .unwrap();
    assert!(!mismatch_output.status.success());
    assert!(String::from_utf8_lossy(&mismatch_output.stderr).contains("exact detected command"));
}

#[test]
fn cli_command_target_preserves_json_argv_without_executing_it() {
    let fixture = tempdir().unwrap();
    let marker = fixture.path().join("must-not-exist");
    let argv = serde_json::to_string(&vec![
        "/usr/bin/touch".to_string(),
        marker.display().to_string(),
    ])
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--no-builtin-targets",
            "--target-argv",
            &argv,
            "--json",
        ])
        .env("HOME", fixture.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["targets"][0]["operation"]["command"]["args"][0],
        marker.display().to_string()
    );
    assert!(
        !marker.exists(),
        "planned argv must never execute in Phase 2"
    );
}

#[test]
#[cfg(unix)]
fn configured_path_with_symlinked_ancestor_is_refused() {
    let fixture = tempdir().unwrap();
    let real = fixture.path().join("real/generated");
    fs::create_dir_all(&real).unwrap();
    let link = fixture.path().join("linked");
    std::os::unix::fs::symlink(fixture.path().join("real"), &link).unwrap();
    let config = fixture.path().join("targets.toml");
    fs::write(
        &config,
        format!(
            r#"[[target]]
id = "custom.symlink"
detect = {{ path = {:?} }}
class = "free"
reclaim = {{ method = "delete" }}
"#,
            link.join("generated").display().to_string()
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--no-builtin-targets",
            "--json",
        ])
        .env("HOME", fixture.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        json["targets"][0]["safety"]["reason"]
            .as_str()
            .unwrap()
            .contains("symlinked ancestor")
    );
}
