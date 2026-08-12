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
