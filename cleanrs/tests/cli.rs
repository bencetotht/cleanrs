use std::fs;
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
    assert!(json["targets"][0]["allocated"].as_u64().unwrap() > 0);
    assert!(json["targets"][0]["estimate"]["ceiling"].as_u64().unwrap() > 0);
}

#[test]
fn targets_human_output_is_explicitly_read_only() {
    let fixture = tempdir().unwrap();
    fs::write(fixture.path().join("Cargo.toml"), b"[package]").unwrap();
    fs::create_dir(fixture.path().join("target")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cleanrs"))
        .args(["targets", fixture.path().to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("RECLAIMABLE"));
    assert!(stdout.contains("cargo.target"));
    assert!(stdout.contains("read-only; no action was run"));
    assert!(!stdout.contains("\u{1b}["));
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
        .args(["targets", ".", "--json"])
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
        .args(["targets", fixture.path().to_str().unwrap(), "--json"])
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
