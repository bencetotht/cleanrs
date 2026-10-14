use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use tempfile::tempdir;

// Resolve fixture tools before tests narrow PATH to stub provider directories.
// macOS uses /usr/bin and /bin; NixOS deliberately does not provide those paths.
fn test_tool(name: &str) -> String {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("fixture tool {name} missing from PATH"))
        .display()
        .to_string()
}

fn rewrite_fixture_tools(path: &std::path::Path) {
    let mut text = fs::read_to_string(path).unwrap();
    for tool in ["rmdir", "touch", "false", "rm", "mv", "mkdir"] {
        for prefix in ["/usr/bin/", "/bin/"] {
            text = text.replace(
                &format!("{prefix}{tool}"),
                &format!("__CLEANRS_FIXTURE_{tool}__"),
            );
        }
    }
    for tool in ["rmdir", "touch", "false", "rm", "mv", "mkdir"] {
        text = text.replace(&format!("__CLEANRS_FIXTURE_{tool}__"), &test_tool(tool));
    }
    fs::write(path, text).unwrap();
}

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
    assert!(stdout.contains("TIP This was a dry run. Add --apply"));
}

#[test]
fn dry_run_tip_suggests_required_level_before_apply() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("package.json"), b"{}").unwrap();
    fs::create_dir(fixture.path().join("node_modules")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--workspace-only",
            "--ignore-staleness",
        ])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("TIP No actions were selected. Try --level standard"));
    assert!(stdout.contains("then add --apply"));
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
fn clean_apply_deletes_only_the_selected_sandbox_artifact_and_journals_it() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("package.json"), b"{}").unwrap();
    fs::create_dir(fixture.path().join(".next")).unwrap();
    let payload = fixture.path().join(".next/payload");
    fs::write(&payload, b"generated fixture").unwrap();
    let state = fixture.path().join("state");

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--workspace-only",
            "--only",
            "node.next",
            "--ignore-staleness",
            "--apply",
            "--yes",
            "--json",
        ])
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!payload.exists());
    assert!(fixture.path().join("package.json").exists());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["execution"], "success");
    assert_eq!(json["results"][0]["result"]["status"], "success");
    assert!(state.join("cleanrs/history.jsonl").exists());
}

#[test]
#[cfg(unix)]
fn cargo_provider_command_revalidates_and_cleans_only_the_fixture_target() {
    let fixture = tempdir().unwrap();
    let scope = fixture.path().canonicalize().unwrap();
    let fake_cargo_bin = install_fake_cargo(&scope);
    let command_log = scope.join("cargo-command.log");
    fs::create_dir(scope.join("src")).unwrap();
    fs::write(
        scope.join("Cargo.toml"),
        "[package]\nname = \"phase3-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        scope.join("Cargo.lock"),
        "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"phase3-fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(scope.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    fs::create_dir(scope.join("target")).unwrap();
    fs::write(scope.join("target/generated"), b"generated").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_cleanrs"));
    command
        .args([
            "clean",
            scope.to_str().unwrap(),
            "--workspace-only",
            "--only",
            "cargo.target",
            "--level",
            "aggressive",
            "--ignore-staleness",
            "--apply",
            "--yes",
            "--json",
        ])
        .env("HOME", &scope)
        .env("XDG_STATE_HOME", scope.join("state"))
        .env("PATH", fake_cargo_bin)
        .env("FAKE_CARGO_LOG", &command_log)
        .env("FAKE_CARGO_ALLOWED_TARGET", scope.join("target"));
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!scope.join("target").exists());
    assert!(scope.join("Cargo.toml").exists());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"][0]["result"]["status"], "success");
    assert_eq!(json["targets"][0]["operation"]["method"], "command");
    assert_eq!(
        fs::read_to_string(command_log).unwrap(),
        format!(
            "clean\n--frozen\n--manifest-path\n{}\n--target-dir\n{}\n",
            scope.join("Cargo.toml").display(),
            scope.join("target").display()
        )
    );
}

#[cfg(unix)]
fn install_fake_cargo(root: &std::path::Path) -> std::path::PathBuf {
    let bin = root.join("cargo-bin");
    fs::create_dir(&bin).unwrap();
    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        r#"#!/bin/sh
set -eu

case "${1-}" in
  --version)
    echo "cargo 1.85.0 (cleanrs fixture)"
    ;;
  metadata)
    manifest=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--manifest-path" ]; then
        manifest="$2"
        shift 2
      else
        shift
      fi
    done
    root=${manifest%/Cargo.toml}
    printf '{"workspace_root":"%s","target_directory":"%s/target"}\n' "$root" "$root"
    ;;
  clean)
    printf '%s\n' "$@" > "$FAKE_CARGO_LOG"
    target=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--target-dir" ]; then
        target="$2"
        shift 2
      else
        shift
      fi
    done
    [ "$target" = "$FAKE_CARGO_ALLOWED_TARGET" ] || exit 64
    /bin/rm -- "$target/generated"
    /bin/rmdir -- "$target"
    ;;
  *)
    exit 64
    ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&cargo).unwrap().permissions();
    permissions.set_mode(0o755);
    rewrite_fixture_tools(&cargo);
    fs::set_permissions(&cargo, permissions).unwrap();
    bin
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
    rewrite_fixture_tools(&brew);
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
#[cfg(unix)]
fn built_in_provider_executes_exact_reviewed_argv_and_records_report() {
    let fixture = tempdir().unwrap();
    let bin = fixture.path().join("bin");
    let marker = fixture.path().join("brew-applied");
    fs::create_dir(&bin).unwrap();
    let brew = bin.join("brew");
    fs::write(
        &brew,
        format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "Homebrew 6.0.18"
elif [ "$1" = "--prefix" ]; then
  echo "/fixture/homebrew"
elif [ "$1" = "--cache" ]; then
  echo "/fixture/cache"
elif [ "$1" = "cleanup" ] && [ "$2" = "--dry-run" ]; then
  echo "This operation would free approximately 12MB of disk space."
elif [ "$1" = "cleanup" ]; then
  /usr/bin/touch {:?}
  echo "Freed 12MB"
else
  exit 2
fi
"#,
            marker.display().to_string()
        ),
    )
    .unwrap();
    rewrite_fixture_tools(&brew);
    fs::set_permissions(&brew, fs::Permissions::from_mode(0o755)).unwrap();
    let state = fixture.path().join("state");
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--only",
            "homebrew.cleanup",
            "--level",
            "standard",
            "--apply",
            "--yes",
            "--json",
        ])
        .env("PATH", &bin)
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(marker.exists());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["results"][0]["result"]["provider_reported"],
        12_000_000
    );
}

#[test]
#[cfg(unix)]
fn provider_identity_change_aborts_without_running_cleanup() {
    let fixture = tempdir().unwrap();
    let bin = fixture.path().join("bin");
    let marker = fixture.path().join("must-not-run");
    let counter = fixture.path().join("prefix-count");
    fs::create_dir(&bin).unwrap();
    let brew = bin.join("brew");
    fs::write(
        &brew,
        format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "Homebrew 6.0.18"
elif [ "$1" = "--prefix" ]; then
  count=0
  if [ -f {:?} ]; then read count < {:?}; fi
  count=$((count + 1))
  echo "$count" > {:?}
  if [ "$count" = "1" ]; then echo "/fixture/first"; else echo "/fixture/changed"; fi
elif [ "$1" = "--cache" ]; then
  echo "/fixture/cache"
elif [ "$1" = "cleanup" ] && [ "$2" = "--dry-run" ]; then
  echo "This operation would free approximately 1MB of disk space."
elif [ "$1" = "cleanup" ]; then
  /usr/bin/touch {:?}
else
  exit 2
fi
"#,
            counter.display().to_string(),
            counter.display().to_string(),
            counter.display().to_string(),
            marker.display().to_string(),
        ),
    )
    .unwrap();
    rewrite_fixture_tools(&brew);
    fs::set_permissions(&brew, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--only",
            "homebrew.cleanup",
            "--level",
            "standard",
            "--apply",
            "--yes",
            "--json",
        ])
        .env("PATH", &bin)
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", fixture.path().join("state"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"][0]["result"]["status"], "validation_abort");
    assert!(
        json["results"][0]["result"]["error"]
            .as_str()
            .unwrap()
            .contains("identity or context changed")
    );
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
fn explicit_config_path_can_apply_only_inside_its_sandbox_scope() {
    let fixture = tempdir().unwrap();
    let scope = fixture.path().canonicalize().unwrap();
    let artifact = scope.join("generated");
    fs::create_dir(&artifact).unwrap();
    fs::write(artifact.join("payload"), b"generated").unwrap();
    let survivor = scope.join("survivor");
    fs::write(&survivor, b"must survive").unwrap();
    let config = scope.join("targets.toml");
    fs::write(
        &config,
        format!(
            r#"[[target]]
id = "custom.generated"
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
            "clean",
            scope.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--no-builtin-targets",
            "--apply",
            "--yes",
            "--json",
        ])
        .env("HOME", &scope)
        .env("XDG_STATE_HOME", scope.join("state"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!artifact.exists());
    assert!(survivor.exists());
    assert!(config.exists());
}

#[test]
fn history_retains_skipped_plan_decisions() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("package.json"), b"{}").unwrap();
    fs::create_dir(fixture.path().join(".next")).unwrap();
    fs::create_dir(fixture.path().join("node_modules")).unwrap();
    let state = fixture.path().join("state");
    let apply = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--workspace-only",
            "--ignore-staleness",
            "--apply",
            "--yes",
            "--json",
        ])
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    let history = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["history", "--json"])
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    let skipped = json[0]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["target_id"] == "node.modules")
        .unwrap();
    assert_eq!(skipped["status"], "skipped");
    assert!(skipped["error"].as_str().unwrap().contains("cleanup level"));
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
fn explicit_config_command_requires_persistent_content_hash_trust() {
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("trust-config"));
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
    let argv =
        serde_json::to_string(&vec![test_tool("touch"), marker.display().to_string()]).unwrap();
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

#[test]
#[cfg(unix)]
fn trusted_config_hash_is_persistent_private_and_invalidated_by_edits() {
    let fixture = tempdir().unwrap();
    let config = fixture.path().join("commands.toml");
    fs::write(
        &config,
        r#"[[target]]
id = "custom.command"
detect = { command = "/usr/bin/touch" }
class = "free"
reclaim = { method = "run", argv = ["/usr/bin/touch", "/tmp/never-run-by-targets"] }
"#,
    )
    .unwrap();
    let trust = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["trust-config", config.to_str().unwrap(), "--json"])
        .env("HOME", fixture.path())
        .output()
        .unwrap();
    assert!(
        trust.status.success(),
        "{}",
        String::from_utf8_lossy(&trust.stderr)
    );
    let store = fixture.path().join(".config/cleanrs/trusted-configs.json");
    assert_eq!(
        fs::metadata(&store).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let trusted = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
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
        trusted.status.success(),
        "{}",
        String::from_utf8_lossy(&trusted.stderr)
    );

    use std::io::Write as _;
    writeln!(fs::OpenOptions::new().append(true).open(&config).unwrap()).unwrap();
    let edited = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "targets",
            fixture.path().to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--no-builtin-targets",
        ])
        .env("HOME", fixture.path())
        .output()
        .unwrap();
    assert!(!edited.status.success());
    assert!(String::from_utf8_lossy(&edited.stderr).contains("current content is not trusted"));
}

#[test]
fn ad_hoc_argv_executes_without_a_shell_and_appears_in_history() {
    let fixture = tempdir().unwrap();
    let marker = fixture.path().join("command-ran");
    let state = fixture.path().join("state");
    let argv =
        serde_json::to_string(&vec![test_tool("touch"), marker.display().to_string()]).unwrap();
    let apply = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--targets-only-cli",
            "--target-argv",
            &argv,
            "--apply",
            "--yes",
            "--json",
        ])
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(marker.exists());

    let history = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["history", "--json"])
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(history.status.success());
    let json: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(
        json.as_array().unwrap().len(),
        1,
        "snapshots must collapse by run ID"
    );
    assert_eq!(json[0]["actions"][0]["status"], "success");
    assert_eq!(json[0]["actions"][0]["argv"][0], test_tool("touch"));
}

#[test]
fn partial_command_failure_continues_and_is_journalled() {
    let fixture = tempdir().unwrap();
    let marker = fixture.path().join("second-command-ran");
    let state = fixture.path().join("state");
    let failing = serde_json::to_string(&vec![test_tool("false")]).unwrap();
    let succeeding =
        serde_json::to_string(&vec![test_tool("touch"), marker.display().to_string()]).unwrap();
    let apply = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--targets-only-cli",
            "--target-argv",
            &failing,
            "--target-argv",
            &succeeding,
            "--apply",
            "--yes",
            "--json",
        ])
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(!apply.status.success());
    assert!(
        marker.exists(),
        "a failed action must not hide later independent work"
    );
    let json: serde_json::Value = serde_json::from_slice(&apply.stdout).unwrap();
    assert_eq!(json["execution"], "partial");
    let statuses: Vec<_> = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["result"]["status"].as_str().unwrap())
        .collect();
    assert!(statuses.contains(&"failed"));
    assert!(statuses.contains(&"success"));
}

#[test]
#[cfg(unix)]
fn filesystem_replacement_abort_is_partial_and_journalled() {
    let fixture = tempdir().unwrap();
    let scope = fixture.path().canonicalize().unwrap();
    let target = scope.join("generated");
    fs::create_dir(&target).unwrap();
    let replacer = scope.join("replace-target");
    fs::write(
        &replacer,
        format!(
            "#!/bin/sh\n/bin/mv {:?} {:?}\n/bin/mkdir {:?}\n/usr/bin/touch {:?}\n",
            target.display().to_string(),
            scope.join("old-generated").display().to_string(),
            target.display().to_string(),
            target.join("must-survive").display().to_string(),
        ),
    )
    .unwrap();
    rewrite_fixture_tools(&replacer);
    fs::set_permissions(&replacer, fs::Permissions::from_mode(0o755)).unwrap();
    let config = scope.join("targets.toml");
    fs::write(
        &config,
        format!(
            r#"[[target]]
id = "custom.command"
detect = {{ command = {:?} }}
class = "free"
reclaim = {{ method = "run", argv = [{:?}] }}

[[target]]
id = "custom.path"
detect = {{ path = {:?} }}
class = "free"
reclaim = {{ method = "delete" }}
stale_after = "0d"
"#,
            replacer.display().to_string(),
            replacer.display().to_string(),
            target.display().to_string(),
        ),
    )
    .unwrap();
    let state = scope.join("state");
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            scope.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--trust-config-commands",
            "--no-builtin-targets",
            "--apply",
            "--yes",
            "--json",
        ])
        .env("HOME", &scope)
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(target.join("must-survive").exists());
    assert!(scope.join("old-generated").exists());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["execution"], "partial");
    assert_eq!(json["results"][0]["result"]["status"], "success");
    assert_eq!(json["results"][1]["result"]["status"], "validation_abort");
    let history = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["history", "--json"])
        .env("HOME", &scope)
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    let history: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(history[0]["outcome"], "partial");
    assert_eq!(history[0]["actions"][1]["status"], "validation_abort");
}

#[test]
fn trash_is_explicitly_deferred_and_json_apply_requires_yes() {
    let fixture = tempdir().unwrap();
    let trash = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--apply",
            "--trash",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(!trash.status.success());
    assert!(String::from_utf8_lossy(&trash.stderr).contains("intentionally deferred"));

    let json = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--apply",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!json.status.success());
    assert!(String::from_utf8_lossy(&json.stderr).contains("requires --yes"));
}

#[test]
fn explicitly_named_user_data_action_cannot_bypass_interactive_review_with_yes() {
    let fixture = tempdir().unwrap();
    let marker = fixture.path().join("must-not-run");
    let config = fixture.path().join("targets.toml");
    fs::write(
        &config,
        format!(
            r#"[[target]]
id = "custom.user-data"
detect = {{ command = "/usr/bin/touch" }}
class = "irreplaceable"
reclaim = {{ method = "run", argv = ["/usr/bin/touch", {:?}] }}
"#,
            marker.display().to_string()
        ),
    )
    .unwrap();
    rewrite_fixture_tools(&config);
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            fixture.path().to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--trust-config-commands",
            "--no-builtin-targets",
            "--only",
            "custom.user-data",
            "--level",
            "aggressive",
            "--apply",
            "--yes",
            "--json",
        ])
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", fixture.path().join("state"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive confirmation"));
    assert!(!marker.exists());
}

fn manual_selection_fixture() -> tempfile::TempDir {
    let fixture = tempdir().unwrap();
    for project in ["chosen", "sibling"] {
        fs::create_dir_all(fixture.path().join(project).join("node_modules")).unwrap();
        fs::write(fixture.path().join(project).join("package.json"), b"{}").unwrap();
        fs::write(
            fixture.path().join(project).join("node_modules/payload"),
            b"generated fixture",
        )
        .unwrap();
    }
    fixture
}

fn manual_selection_command(fixture: &tempfile::TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cleanrs"));
    command
        .arg("clean")
        .arg(fixture.path())
        .arg("--workspace-only")
        .arg("--only")
        .arg("node.modules")
        .arg("--select")
        .arg(fixture.path().join("chosen/node_modules"))
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", fixture.path().join("state"))
        .env("NO_COLOR", "1");
    command
}

#[test]
fn clean_select_dry_run_selects_one_candidate_and_displays_manual_override() {
    let fixture = manual_selection_fixture();
    let output = manual_selection_command(&fixture).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("plan (manual)"));
    assert!(stdout.contains("requires standard cleanup level"));
    assert!(stdout.contains("Only your --select candidates are selected"));
    assert!(!stdout.contains('\u{1b}'));
    let output = manual_selection_command(&fixture)
        .args(["--json", "--level", "aggressive", "--ignore-staleness"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["selected_targets"], 1,
        "--select uses ManualOnly even when the level selects siblings"
    );
    assert_eq!(json["targets"][0]["selection"]["source"]["kind"], "manual");
    assert_eq!(
        json["targets"][0]["selection"]["source"]["overrode"],
        serde_json::Value::Null
    );
    assert_eq!(json["targets"][1]["selection"]["status"], "skipped");
    assert!(fixture.path().join("chosen/node_modules/payload").exists());
    assert!(fixture.path().join("sibling/node_modules/payload").exists());
    assert!(
        !fixture.path().join("state").exists(),
        "dry-run must not journal"
    );
}

#[test]
fn clean_select_apply_deletes_exact_candidate_and_history_marks_override() {
    let fixture = manual_selection_fixture();
    let output = manual_selection_command(&fixture)
        .args(["--apply", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.path().join("chosen/node_modules").exists());
    assert!(fixture.path().join("sibling/node_modules/payload").exists());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 1);
    assert_eq!(json["results"][0]["result"]["status"], "success");
    let history = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .arg("history")
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", fixture.path().join("state"))
        .output()
        .unwrap();
    assert!(history.status.success());
    assert!(
        String::from_utf8_lossy(&history.stdout)
            .contains("[manual: requires standard cleanup level]")
    );
    assert!(!String::from_utf8_lossy(&history.stdout).contains("bytes allocation"));
    let history = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["history", "--json"])
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", fixture.path().join("state"))
        .output()
        .unwrap();
    assert!(history.status.success());
    let runs: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    let action = runs[0]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["status"] == "success")
        .unwrap();
    assert_eq!(action["selection_source"], "manual");
    assert_eq!(
        action["overridden_skip_reason"],
        "requires standard cleanup level"
    );
}

#[test]
fn clean_select_refused_candidate_fails_before_apply() {
    let fixture = tempdir().unwrap();
    let scope = fixture.path().canonicalize().unwrap();
    let spec = format!("{}:cheap:delete", scope.display());
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .arg("clean")
        .arg(fixture.path())
        .args(["--targets-only-cli", "--target"])
        .arg(spec)
        .arg("--select")
        .arg(fixture.path())
        .arg("--json")
        .env("HOME", fixture.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot select"));
    assert!(stderr.contains("safety refusal"));
    assert!(stderr.contains("cannot equal the explicit scan root"));
    assert!(fixture.path().exists());
}

#[test]
fn clean_select_unknown_path_lists_candidates_below_it() {
    let fixture = manual_selection_fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .arg("clean")
        .arg(fixture.path())
        .args(["--workspace-only", "--only", "node.modules", "--select"])
        .arg(fixture.path().join("chosen"))
        .env("HOME", fixture.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown cleanup candidate"));
    assert!(stderr.contains("possible candidates:"));
    assert!(stderr.contains("chosen/node_modules"));
    assert!(!stderr.contains("sibling/node_modules"));
}

#[test]
fn clean_select_is_repeatable_and_canonicalizes_path_spelling() {
    let fixture = manual_selection_fixture();
    let output = manual_selection_command(&fixture)
        .arg("--select")
        .arg(fixture.path().join("sibling/../sibling/node_modules"))
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["selected_targets"], 2);
    assert!(
        json["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|target| target["selection"]["source"]["kind"] == "manual")
    );
}

#[test]
fn clean_select_user_data_still_requires_interactive_confirmation() {
    let fixture = tempdir().unwrap();
    fs::create_dir(fixture.path().join("data")).unwrap();
    fs::write(
        fixture.path().join("data/irreplaceable"),
        b"fixture user data",
    )
    .unwrap();
    let scope = fixture.path().canonicalize().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .arg("clean")
        .arg(fixture.path())
        .arg("--targets-only-cli")
        .arg("--target")
        .arg(format!(
            "{}:irreplaceable:delete",
            scope.join("data").display()
        ))
        .arg("--select")
        .arg(fixture.path().join("data"))
        .args(["--apply", "--yes", "--json"])
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", fixture.path().join("state"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("explicit-only actions require interactive confirmation")
    );
    assert!(fixture.path().join("data/irreplaceable").exists());
    assert!(!fixture.path().join("state").exists());
}

#[test]
fn tui_rejects_non_tty_before_touching_terminal_modes() {
    let fixture = tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .current_dir(fixture.path())
        .arg("tui")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("requires a terminal on both stdin and stdout")
    );
    assert!(
        output.stdout.is_empty(),
        "must not enter the alternate screen"
    );
}

#[test]
fn projects_reports_artifacts_and_evidence_without_executing_or_loading_cwd_policy() {
    let fixture = tempdir().unwrap();
    let project = fixture.path().join("project");
    fs::create_dir_all(project.join(".next")).unwrap();
    fs::create_dir(project.join(".venv")).unwrap();
    fs::write(project.join(".venv/pyvenv.cfg"), "home = /fixture/python").unwrap();
    fs::write(project.join("package.json"), "{}").unwrap();
    fs::write(project.join("pyproject.toml"), "[project]").unwrap();
    fs::write(project.join(".next/output"), "generated").unwrap();
    fs::write(
        project.join("targets.toml"),
        "this is invalid and must never load",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["projects", ".", "--json"])
        .current_dir(&project)
        .env("HOME", fixture.path().join("home"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["projects"].as_array().unwrap().len(), 1);
    let artifacts = json["projects"][0]["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 2);
    assert!(
        artifacts
            .iter()
            .all(|a| a["target"]["usage"]["reclaimable"]["confidence"].is_string())
    );
    assert!(
        artifacts
            .iter()
            .any(|a| a["ecosystem"] == "python" && a["recommended"] == false)
    );
    assert!(project.join(".next/output").exists());
    let human = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["projects", project.to_str().unwrap()])
        .env("HOME", fixture.path().join("home"))
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(
        human.contains("ACTIVITY")
            && human.contains("ATTRIBUTED")
            && human.contains("consequence:")
    );
    assert!(!human.contains('\u{1b}'));
}

#[test]
fn project_retention_is_shared_by_targets_and_clean_and_keep_survives_ignore_staleness() {
    let fixture = tempdir().unwrap();
    let project = fixture.path().join("project");
    fs::create_dir_all(project.join(".next")).unwrap();
    fs::write(project.join("package.json"), "{}").unwrap();
    fs::write(project.join(".next/output"), "generated").unwrap();
    let config = fixture.path().join("policy.toml");
    fs::write(
        &config,
        format!(
            "[[retention]]\npath = {:?}\nartifact_days = 0\ndormant_days = 180\nkeep = true\n",
            project.display().to_string()
        ),
    )
    .unwrap();
    let command = |subcommand: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cleanrs"));
        cmd.arg(subcommand)
            .arg(&project)
            .arg("--workspace-only")
            .arg("--config")
            .arg(&config)
            .arg("--json")
            .env("HOME", fixture.path().join("home"));
        cmd
    };
    let targets = command("targets").output().unwrap();
    assert!(
        targets.status.success(),
        "{}",
        String::from_utf8_lossy(&targets.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&targets.stdout).unwrap();
    assert_eq!(json["projects"]["projects"][0]["protected"], true);
    assert_eq!(json["targets"][0]["stale_after_days"], 0);
    let clean = command("clean")
        .arg("--ignore-staleness")
        .arg("--level")
        .arg("aggressive")
        .output()
        .unwrap();
    assert!(clean.status.success());
    let json: serde_json::Value = serde_json::from_slice(&clean.stdout).unwrap();
    assert_eq!(json["selected_targets"], 0);
    let manual = command("clean")
        .arg("--select")
        .arg(project.join(".next"))
        .output()
        .unwrap();
    assert!(manual.status.success());
    let json: serde_json::Value = serde_json::from_slice(&manual.stdout).unwrap();
    assert_eq!(json["selected_targets"], 1);
    assert_eq!(json["targets"][0]["selection"]["source"]["kind"], "manual");
    assert!(project.join(".next/output").exists());
}

#[test]
fn retention_rejects_relative_paths_parent_traversal_and_unknown_fields() {
    let fixture = tempdir().unwrap();
    let config = fixture.path().join("policy.toml");
    for text in [
        "[[retention]]\npath = 'relative'\nartifact_days = 14\n",
        "[[retention]]\npath = '/tmp/../'\nartifact_days = 14\n",
        "[[retention]]\npath = '/tmp'\nartifact_days = 14\nunknown = true\n",
    ] {
        fs::write(&config, text).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
            .arg("projects")
            .arg(fixture.path())
            .arg("--config")
            .arg(&config)
            .arg("--json")
            .env("HOME", fixture.path().join("home"))
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted invalid policy: {text}");
    }
}

#[test]
fn system_scope_skips_workspace_targets_and_rejects_paths_or_workspace_only() {
    let fixture = tempdir().unwrap();
    let home = fixture.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(fixture.path().join("package.json"), "{}").unwrap();
    fs::create_dir(fixture.path().join("node_modules")).unwrap();
    for subcommand in ["targets", "clean"] {
        let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
            .args([subcommand, "--system", "--only", "node.modules", "--json"])
            .current_dir(fixture.path())
            .env("HOME", &home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["system"], true);
        assert_eq!(json["scope"], "[system]");
        assert!(json["targets"].as_array().unwrap().is_empty());
        assert!(output.stderr.is_empty());
        for conflict in [".", "--workspace-only"] {
            let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
                .args([subcommand, "--system", conflict])
                .current_dir(fixture.path())
                .env("HOME", &home)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
        }
    }
}

#[cfg(unix)]
#[test]
fn system_go_action_applies_only_to_its_sandbox_and_journals_system_scope() {
    let fixture = tempdir().unwrap();
    let home = fixture.path().join("home");
    let bin = fixture.path().join("bin");
    let cache = home.join("go-build");
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::write(cache.join("payload"), b"cache").unwrap();
    fs::write(home.join("preserved"), b"user data").unwrap();
    let go = bin.join("go");
    fs::write(
        &go,
        format!(
            r#"#!/bin/sh
case "$1" in
  env) printf '%s\n' '{{"GOCACHE":"{}","GOMODCACHE":"{}"}}' ;;
  version) printf '%s\n' 'go version fixture' ;;
  clean)
    test "$2" = '-cache' || exit 1
    test "$GOCACHE" = '{}' || exit 1
    test "$GOTOOLCHAIN" = 'local' || exit 1
    /bin/rm -f "$GOCACHE/payload" ;;
  *) exit 1 ;;
esac
"#,
            cache.display(),
            home.join("go-modules").display(),
            cache.display()
        ),
    )
    .unwrap();
    rewrite_fixture_tools(&go);
    fs::set_permissions(&go, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args([
            "clean",
            "--system",
            "--only",
            "go.build-cache",
            "--apply",
            "--yes",
            "--json",
        ])
        .current_dir(fixture.path())
        .env("HOME", &home)
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!cache.join("payload").exists());
    assert!(home.join("preserved").exists());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["system"], true);
    assert_eq!(json["selected_targets"], 1);
    let history = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["history", "--json"])
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(history.status.success());
    let history: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    let runs = history.as_array().unwrap();
    assert_eq!(runs[0]["system"], true);
    assert_eq!(runs[0]["scope"], "[system]");
    assert_eq!(runs[0]["actions"][0]["target_id"], "go.build-cache");
}
