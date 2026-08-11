use std::cmp::Reverse;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use cleanrs_core::accounting::estimate_subtree;
use cleanrs_core::fs::{Arena, NodeIndex, ScanResult, doctor};
use cleanrs_core::{ReclaimEstimate, ScanOptions, Scanner};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "cleanrs", version, about, propagate_version = true)]
struct Cli {
    /// Increase diagnostic logging (-v or -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Scan an explicit directory without reading file contents.
    Scan(ScanArgs),
    /// Inspect container, volume, snapshot, swap, and access state.
    Doctor(DoctorArgs),
    /// Generate shell completions on standard output.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Generate the cleanrs(1) manual page on standard output.
    Man,
}

#[derive(Debug, clap::Args)]
struct DoctorArgs {
    /// Emit stable machine-readable output.
    #[arg(long)]
    json: bool,

    /// Reconcile one explicit scan scope against the Data volume.
    #[arg(long, value_name = "PATH")]
    scan: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct ScanArgs {
    /// Directory to scan. Defaults to the working directory, never the home directory.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Emit stable machine-readable output.
    #[arg(long)]
    json: bool,

    /// Rank and display logical file length.
    #[arg(long, conflicts_with_all = ["allocated", "private", "reclaimable"])]
    apparent: bool,

    /// Rank and display filesystem allocated bytes (the default).
    #[arg(long, conflicts_with_all = ["apparent", "private", "reclaimable"])]
    allocated: bool,

    /// Display the immediately-private lower bound.
    #[arg(long, conflicts_with_all = ["apparent", "allocated", "reclaimable"])]
    private: bool,

    /// Display the reclaimability interval and derived confidence.
    #[arg(long, conflicts_with_all = ["apparent", "allocated", "private"])]
    reclaimable: bool,

    /// Maximum displayed tree depth. The complete subtree is still scanned.
    #[arg(long, default_value_t = 1)]
    depth: usize,

    /// Hide entries smaller than this value (for example 10MiB or 2GB).
    #[arg(long, value_parser = parse_size, default_value = "0")]
    min_size: u64,

    /// Exclude a glob; may be supplied more than once.
    #[arg(long)]
    exclude: Vec<String>,

    /// Explain the estimate for one path within the scan.
    #[arg(long)]
    explain: Option<PathBuf>,

    /// Traverse mount boundaries. Disabled by default.
    #[arg(long)]
    cross_mounts: bool,

    /// Directory worker count (blocking workers; no async runtime).
    #[arg(long)]
    workers: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
enum Metric {
    Apparent,
    Allocated,
    Private,
    Reclaimable,
}

#[derive(Debug, Serialize)]
struct ScanOutput {
    root: PathBuf,
    metric: &'static str,
    backend: String,
    coverage_complete: bool,
    coverage_errors: Vec<cleanrs_core::fs::CoverageError>,
    dataless_entries: usize,
    mount_boundaries: usize,
    entries: Vec<OutputEntry>,
}

#[derive(Debug, Serialize)]
struct OutputEntry {
    path: PathBuf,
    depth: usize,
    apparent: u64,
    allocated: u64,
    estimate: ReclaimEstimate,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    match cli.command {
        Commands::Scan(args) => run_scan(args),
        Commands::Doctor(args) => run_doctor(args),
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "cleanrs", &mut io::stdout());
            Ok(())
        }
        Commands::Man => {
            clap_mangen::Man::new(Cli::command()).render(&mut io::stdout())?;
            Ok(())
        }
    }
}

fn run_scan(args: ScanArgs) -> Result<()> {
    let metric = selected_metric(&args);
    let mut options = ScanOptions {
        cross_mounts: args.cross_mounts,
        excludes: args.exclude,
        ..ScanOptions::default()
    };
    if let Some(workers) = args.workers {
        if workers == 0 {
            bail!("--workers must be at least 1");
        }
        options.workers = workers;
    }
    let result = Scanner::new(options)
        .scan(&args.path)
        .with_context(|| format!("failed to scan {}", args.path.display()))?;
    let output = build_output(
        &result,
        metric,
        args.depth,
        args.min_size,
        args.explain.as_deref(),
    )?;
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), &output)?;
        println!();
    } else {
        print_scan(&output, metric);
    }
    Ok(())
}

fn selected_metric(args: &ScanArgs) -> Metric {
    if args.apparent {
        Metric::Apparent
    } else if args.private {
        Metric::Private
    } else if args.reclaimable {
        Metric::Reclaimable
    } else {
        Metric::Allocated
    }
}

fn build_output(
    result: &ScanResult,
    metric: Metric,
    max_depth: usize,
    min_size: u64,
    explain: Option<&Path>,
) -> Result<ScanOutput> {
    let coverage_complete = result.coverage_complete();
    let targets = if let Some(path) = explain {
        let requested = if path.is_absolute() {
            path.to_path_buf()
        } else {
            result.root.join(path)
        };
        let requested = requested.canonicalize().unwrap_or(requested);
        let index = (0..result.arena.nodes.len())
            .map(NodeIndex::from_usize)
            .find(|index| result.arena.path(*index) == requested)
            .with_context(|| format!("{} is not part of this scan", requested.display()))?;
        vec![(index, depth_of(&result.arena, index))]
    } else {
        nodes_through_depth(&result.arena, max_depth)
    };

    let mut entries: Vec<_> = targets
        .into_iter()
        .map(|(index, depth)| {
            let (apparent, allocated) = subtree_sizes(&result.arena, index);
            OutputEntry {
                path: result.arena.path(index),
                depth,
                apparent,
                allocated,
                estimate: estimate_subtree(&result.arena, index, coverage_complete),
            }
        })
        .filter(|entry| metric_value(entry, metric) >= min_size)
        .collect();
    entries.sort_by(|left, right| {
        Reverse(metric_value(left, metric))
            .cmp(&Reverse(metric_value(right, metric)))
            .then_with(|| left.path.cmp(&right.path))
    });

    Ok(ScanOutput {
        root: result.root.clone(),
        metric: metric_name(metric),
        backend: result.backend.clone(),
        coverage_complete,
        coverage_errors: result.coverage_errors.clone(),
        dataless_entries: result.dataless_entries,
        mount_boundaries: result.mount_boundaries,
        entries,
    })
}

fn print_scan(output: &ScanOutput, metric: Metric) {
    println!("{} scan · {}", metric_name(metric), output.root.display());
    for entry in &output.entries {
        let indent = "  ".repeat(entry.depth);
        match metric {
            Metric::Apparent => println!(
                "{}{:>10}  {}",
                indent,
                human_bytes(entry.apparent),
                entry.path.display()
            ),
            Metric::Allocated => println!(
                "{}{:>10}  {}",
                indent,
                human_bytes(entry.allocated),
                entry.path.display()
            ),
            Metric::Private => println!(
                "{}{:>10}  {}",
                indent,
                entry
                    .estimate
                    .floor
                    .map_or_else(|| "unknown".into(), human_bytes),
                entry.path.display()
            ),
            Metric::Reclaimable => println!(
                "{}{:>21} · {:?}  {}",
                indent,
                format_interval(&entry.estimate),
                entry.estimate.confidence,
                entry.path.display()
            ),
        }
        if matches!(metric, Metric::Private | Metric::Reclaimable) {
            if let Some(explanation) = &entry.estimate.explanation {
                println!("{}  {}", indent, explanation);
            }
        }
    }
    if output.dataless_entries > 0 {
        println!(
            "\nSafety: {} dataless entries observed; content was never opened.",
            output.dataless_entries
        );
    }
    if !output.coverage_complete {
        println!(
            "\nCoverage incomplete: {} inaccessible locations (size unknown).",
            output.coverage_errors.len()
        );
        for error in output.coverage_errors.iter().take(10) {
            println!("  {}: {}", error.path.display(), error.message);
        }
    }
    println!("\nBackend: {}", output.backend);
}

fn run_doctor(args: DoctorArgs) -> Result<()> {
    let mut report = doctor().context("failed to inspect filesystem state")?;
    if let Some(path) = args.scan {
        let scan = Scanner::new(ScanOptions::default())
            .scan(&path)
            .with_context(|| format!("failed to scan {} for reconciliation", path.display()))?;
        let estimate = estimate_subtree(&scan.arena, scan.arena.root(), scan.coverage_complete());
        report.attributed_allocated = estimate.ceiling;
        report.scan_coverage_complete = Some(scan.coverage_complete());
        let data_used = report
            .volumes
            .iter()
            .find(|volume| volume.name == "Data")
            .map_or(report.used, |volume| volume.used);
        report.unattributed = estimate
            .ceiling
            .map(|value| data_used.saturating_sub(value));
        report.notes.push(format!(
            "Attributed values cover only the explicit scan scope {}.",
            scan.root.display()
        ));
        if !scan.coverage_complete() {
            report.notes.push(format!(
                "Scan coverage is incomplete at {} locations; their size is unknown.",
                scan.coverage_errors.len()
            ));
        }
    }
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), &report)?;
        println!();
        return Ok(());
    }
    println!(
        "Container  {}",
        report.container.as_deref().unwrap_or("unavailable")
    );
    println!("  Capacity       {:>12}", human_bytes(report.capacity));
    println!("  Free (shared)  {:>12}", human_bytes(report.free_shared));
    println!("  Used           {:>12}", human_bytes(report.used));
    println!("\nVolumes");
    for volume in &report.volumes {
        println!(
            "  {:<8} {:>12}  {:<20} {}",
            volume.name,
            human_bytes(volume.used),
            volume.mount_point.display(),
            volume.role
        );
    }
    println!("\nData volume, attributed");
    println!(
        "  Files (clone-deduped allocated)  {}",
        report
            .attributed_allocated
            .map_or_else(|| "not scanned".into(), human_bytes)
    );
    println!(
        "  Unattributed remainder           {}",
        report
            .unattributed
            .map_or_else(|| "unknown".into(), human_bytes)
    );
    println!("\nContainer-level, not attributable to paths");
    println!(
        "  Purgeable      {}",
        report
            .purgeable
            .map_or_else(|| "unknown".into(), human_bytes)
    );
    println!(
        "  Snapshots      {}",
        report
            .snapshot_count
            .map_or_else(|| "unknown".into(), |value| value.to_string())
    );
    println!("\nFull Disk Access: {}", report.full_disk_access);
    for note in report.notes {
        println!("  Note: {note}");
    }
    Ok(())
}

fn nodes_through_depth(arena: &Arena, max_depth: usize) -> Vec<(NodeIndex, usize)> {
    let mut output = Vec::new();
    let mut queue = std::collections::VecDeque::from([(arena.root(), 0)]);
    while let Some((index, depth)) = queue.pop_front() {
        output.push((index, depth));
        if depth < max_depth {
            queue.extend(arena.children(index).map(|child| (child, depth + 1)));
        }
    }
    output
}

fn depth_of(arena: &Arena, index: NodeIndex) -> usize {
    let mut depth = 0;
    let mut cursor = index;
    while cursor != arena.root() {
        depth += 1;
        cursor = arena.parent(cursor).expect("non-root node has parent");
    }
    depth
}

fn subtree_sizes(arena: &Arena, root: NodeIndex) -> (u64, u64) {
    use std::collections::HashSet;
    let mut stack = vec![root];
    let mut apparent = 0_u64;
    let mut allocated = 0_u64;
    let mut allocated_inodes = HashSet::new();
    while let Some(index) = stack.pop() {
        let node = &arena.nodes[index.as_usize()];
        stack.extend(arena.children(index));
        if node.kind.is_file_like() {
            apparent = apparent.saturating_add(node.apparent_size);
            if allocated_inodes.insert((node.device, node.inode)) {
                allocated = allocated.saturating_add(node.allocated_size);
            }
        }
    }
    (apparent, allocated)
}

fn metric_value(entry: &OutputEntry, metric: Metric) -> u64 {
    match metric {
        Metric::Apparent => entry.apparent,
        Metric::Allocated => entry.allocated,
        Metric::Private => entry.estimate.floor.unwrap_or_default(),
        Metric::Reclaimable => entry.estimate.ceiling.unwrap_or_default(),
    }
}

const fn metric_name(metric: Metric) -> &'static str {
    match metric {
        Metric::Apparent => "apparent",
        Metric::Allocated => "allocated",
        Metric::Private => "immediately private",
        Metric::Reclaimable => "estimated reclaimable",
    }
}

fn format_interval(estimate: &ReclaimEstimate) -> String {
    match (estimate.floor, estimate.ceiling) {
        (Some(floor), Some(ceiling)) if floor == ceiling => human_bytes(floor),
        (Some(floor), Some(ceiling)) => format!("{}–{}", human_bytes(floor), human_bytes(ceiling)),
        (None, Some(ceiling)) => format!("unknown–{}", human_bytes(ceiling)),
        _ => "unknown".into(),
    }
}

fn parse_size(value: &str) -> Result<u64, String> {
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let number: u64 = value[..split]
        .parse()
        .map_err(|_| "size must start with an integer")?;
    let suffix = value[split..].trim().to_ascii_lowercase();
    let multiplier = match suffix.as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1_u64 << 40,
        _ => return Err(format!("unsupported size suffix {suffix:?}")),
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| "size is too large".into())
}

fn human_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut scaled = value as f64;
    let mut unit = 0;
    while scaled >= 1024.0 && unit < UNITS.len() - 1 {
        scaled /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{scaled:.1} {}", UNITS[unit])
    }
}

fn init_logging(verbose: u8) {
    let fallback = match verbose {
        0 => "warn",
        1 => "cleanrs=info,cleanrs_core=info",
        _ => "cleanrs=debug,cleanrs_core=debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(fallback)),
        )
        .with_writer(io::stderr)
        .with_ansi(std::env::var_os("NO_COLOR").is_none())
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binary_and_decimal_sizes() {
        assert_eq!(parse_size("2MiB"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_size("3GB"), Ok(3_000_000_000));
    }

    #[test]
    fn command_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
