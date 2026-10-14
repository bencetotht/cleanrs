//! Reproducible Phase 4 benchmark, entirely inside a unique temporary tree.
//! Usage: cargo run --release -p cleanrs-core --example project_hygiene -- 100 50 3

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use cleanrs_core::projects::inspect_projects;
use cleanrs_core::providers::SystemProviderHost;
use cleanrs_core::targets::{BUILTIN_TARGETS, detect_workspace_targets};
use cleanrs_core::{ScanOptions, Scanner};

fn git(root: &Path, args: &[&str]) -> Result<(), Box<dyn Error>> {
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
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let repositories = args.first().map_or(Ok(100), |s| s.parse::<usize>())?;
    let files = args.get(1).map_or(Ok(50), |s| s.parse::<usize>())?;
    let runs = args.get(2).map_or(Ok(3), |s| s.parse::<usize>())?;
    let fixture = tempfile::tempdir()?;
    for repo in 0..repositories {
        let root = fixture.path().join(format!("project-{repo:04}"));
        fs::create_dir_all(root.join(".next"))?;
        fs::create_dir(root.join("src"))?;
        fs::write(root.join("package.json"), "{}")?;
        fs::write(root.join(".gitignore"), ".next/\n")?;
        for file in 0..files {
            fs::write(
                root.join("src").join(format!("{file:04}.js")),
                "source fixture\n",
            )?;
            fs::write(
                root.join(".next").join(format!("{file:04}.js")),
                "generated fixture\n",
            )?;
        }
        git(&root, &["init", "-q", "--initial-branch=main"])?;
        git(&root, &["add", "."])?;
        git(&root, &["commit", "-q", "-m", "fixture"])?;
    }
    let scanner = Scanner::new(ScanOptions::default());
    for run in 1..=runs {
        let start = Instant::now();
        let scan = scanner.scan(fixture.path())?;
        let scan_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let mut targets = detect_workspace_targets(&scan, BUILTIN_TARGETS, None);
        let detection_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let inventory = inspect_projects(
            &scan,
            &mut targets,
            &[],
            &SystemProviderHost::project_inspection(),
            Default::default(),
        );
        let hygiene_ms = start.elapsed().as_secs_f64() * 1000.0;
        #[cfg(not(target_os = "linux"))]
        let peak_rss_kib = {
            // ru_maxrss is a process high-water mark, including fixture setup.
            let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
            if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } == 0 {
                let rss = usage.ru_maxrss as u64;
                Some(if cfg!(target_os = "macos") {
                    rss / 1024
                } else {
                    rss
                })
            } else {
                None
            }
        };
        #[cfg(target_os = "linux")]
        let peak_rss_kib: Option<u64> = {
            // ru_maxrss may retain the launcher's high-water mark across exec;
            // VmHWM describes this executable's own address space.
            fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|text| {
                    text.lines().find_map(|line| {
                        line.strip_prefix("VmHWM:")
                            .and_then(|value| value.split_whitespace().next()?.parse().ok())
                    })
                })
        };
        println!(
            "{}",
            serde_json::json!({ "run": run, "repositories": repositories, "source_files_per_repo": files,
            "artifact_files_per_repo": files, "entries": scan.arena.nodes.len(), "scan_ms": scan_ms,
            "detection_ms": detection_ms, "hygiene_ms": hygiene_ms, "peak_rss_kib": peak_rss_kib,
            "inspection_commands": inventory.inspection_commands, "inspected": inventory.inspected_repositories,
            "omitted": inventory.omitted_repositories })
        );
    }
    Ok(())
}
