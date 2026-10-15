//! Provider cache accounting and capability-specific command plans. Occupied
//! storage remains separate from partial native garbage-collection yield.

use super::*;
use crate::fs::{FileSystem, NativeFileSystem, NodeFlags};
use crate::targets::StorageMeasurement;

fn query(host: &dyn ProviderHost, executable: &Path, args: &[&str]) -> Result<String, String> {
    successful_stdout(host, &plain_inspection_command(executable, args)).map_err(|e| e.to_string())
}

fn command(item: &mut DetectedTarget) -> Option<&mut CommandSpec> {
    match &mut item.operation {
        PlannedOperation::Command { command } => Some(command),
        _ => None,
    }
}

fn failed(item: &mut DetectedTarget, reason: impl Into<String>) {
    let reason = reason.into();
    item.safety = CandidateSafety::Refused(reason.clone());
    item.usage = UsageEstimate::Unknown {
        explanation: format!("inspection failed: {reason}"),
    };
    item.evidence.push(reason);
}

/// Variants have different global scopes. Never assume executable detection
/// alone proves a usable cache or silently clean a repository-local Yarn cache.
pub(super) fn inspect_action(
    host: &dyn ProviderHost,
    identity: &ProviderIdentity,
    executable: &Path,
    original: &SimpleAction,
) -> DetectedTarget {
    let mut action = *original;
    match action.id {
        "yarn.cache-clean" => {
            action.location_args = match identity.version.as_deref().and_then(version_major) {
                Some(1) => &["cache", "dir"],
                Some(2..) => &["config", "get", "globalFolder", "--json"],
                _ => {
                    let mut item = inspect_simple_action_base(host, identity, executable, original);
                    item.operation = PlannedOperation::Informational;
                    failed(
                        &mut item,
                        "Yarn variant is unavailable with Corepack network access disabled",
                    );
                    return item;
                }
            };
            if identity.version.as_deref().and_then(version_major) != Some(1) {
                action.plan_args = &["cache", "clean", "--mirror"];
            }
        }
        "npm.npx-cache"
            if query(host, executable, &["cache", "--help"])
                .is_ok_and(|text| text.contains("npx rm")) =>
        {
            // Read the installed command's usage instead of assuming all npm
            // versions implement the newer, separate execution-cache command.
            action.informational = false;
            action.plan_args = &["cache", "npx", "rm", "--force"];
            action.consequence =
                "npx execution packages are removed and will be downloaded on next use";
        }
        "poetry.cache-clean" => {
            action.plan_args = &["cache", "clear", "PyPI", "--all", "--no-interaction"];
        }
        "poetry.environments" => {
            // `poetry env list` is project-scoped. Global inventory instead
            // enumerates only the explicitly configured environment directory.
            action.inspect_args = &[];
        }
        "conda.cache-clean" => {
            // Package-cache pruning can break environments that symlink into
            // the cache. Limit routine cleanup to disposable downloads/indexes.
            action.location_args = &[];
            action.inspect_args = &[
                "clean",
                "--tarballs",
                "--index-cache",
                "--logfiles",
                "--dry-run",
                "--json",
            ];
            action.plan_args = &[
                "clean",
                "--tarballs",
                "--index-cache",
                "--logfiles",
                "--yes",
                "--json",
            ];
            action.description = "remove Conda download archives, indexes and logs";
            action.consequence = "download archives, indexes and logs are removed; extracted packages are preserved to protect symlinked environments";
        }
        "conda.environments" => {
            action.location_args = &[];
            action.inspect_args = &[];
        }
        "cocoapods.cache-clean" => {
            // `pod cache list` is a listing, never an absolute directory.
            // Its initializer may replace old caches, so do not invoke it
            // during metadata-only inventory.
            action.location_args = &[];
            action.inspect_args = &[];
        }
        _ => {}
    }
    let mut item = inspect_simple_action_base(host, identity, executable, &action);
    if let Some(spec) = command(&mut item) {
        // Apply uses the same neutral working directory and shim constraints
        // as inspection; these overrides are also part of reviewed identity.
        spec.environment
            .extend(plain_inspection_command(executable, &[]).environment);
    }
    if action.id == "bun.cache-clean" && matches!(item.safety, CandidateSafety::Refused(_)) {
        // A failed native location query never authorizes guessed cleanup.
        // Still expose a known configured/default cache as inventory evidence.
        if let Some(path) = home_path(host, "BUN_INSTALL_CACHE_DIR", ".bun/install/cache") {
            item.evidence.push(format!(
                "unvalidated Bun fallback cache location: {}",
                path.display()
            ));
        }
    }
    item
}

pub(super) fn version_major(version: &str) -> Option<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Parse provider reports deliberately; an unsupported format is unknown, not
/// zero bytes. Conda totals cover tarballs only (indexes/logs have no byte total).
pub(super) fn reported_bytes(id: &str, output: &str) -> Option<u64> {
    match id {
        "pip.cache-purge" => {
            let mut total = 0_u64;
            let mut seen = 0;
            for line in output.lines() {
                let Some((label, value)) = line.rsplit_once(':') else {
                    continue;
                };
                if label.starts_with("Package index page cache size")
                    || label == "Locally built wheels size"
                {
                    total = total.checked_add(parse_human_bytes(value.trim())?)?;
                    seen += 1;
                }
            }
            (seen == 2).then_some(total)
        }
        "conda.cache-clean" => None,
        _ => output.trim().parse().ok(),
    }
}

pub(super) fn inspection_evidence(id: &str, output: &str) -> Result<Vec<String>, String> {
    if id != "conda.cache-clean" {
        return Ok(vec![]);
    }
    let value: serde_json::Value =
        serde_json::from_str(output).map_err(|e| format!("invalid Conda dry-run JSON: {e}"))?;
    if value["success"].as_bool() != Some(true) {
        return Err("Conda dry-run did not report success".into());
    }
    let bytes = value["tarballs"]["total_size"]
        .as_u64()
        .ok_or("Conda dry-run has no numeric tarball total_size")?;
    let mut evidence = vec![format!(
        "Conda dry-run tarball subtotal: {bytes} bytes; index/log cleanup size remains unknown"
    )];
    if let Some(warnings) = value["tarballs"]["warnings"].as_array() {
        for warning in warnings.iter().filter_map(|w| w.as_str()) {
            evidence.push(format!("Conda inspection warning: {warning}"));
        }
    }
    Ok(evidence)
}

fn absolute_location(value: &str) -> Option<PathBuf> {
    // JSON strings (modern Yarn) and plain single-line paths are accepted;
    // multi-line diagnostics can never become deletion paths.
    let value = value.trim();
    let decoded = serde_json::from_str::<String>(value).unwrap_or_else(|_| value.into());
    let path = PathBuf::from(decoded);
    (path.is_absolute() && !value.contains('\n')).then_some(path)
}

fn json_paths(value: &serde_json::Value, key: &str) -> Result<Vec<PathBuf>, String> {
    let entries = value[key]
        .as_array()
        .ok_or_else(|| format!("provider JSON has no {key} array"))?;
    if entries.len() > 128 {
        return Err(format!("provider {key} inventory exceeds 128 roots"));
    }
    entries
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .and_then(absolute_location)
                .ok_or_else(|| format!("provider {key} contains a non-absolute or invalid path"))
        })
        .collect()
}

fn home_path(host: &dyn ProviderHost, variable: &str, suffix: &str) -> Option<PathBuf> {
    host.variable(variable)
        .map(PathBuf::from)
        .or_else(|| host.home().map(|p| p.join(suffix)))
}

/// Support the normal scalar cache_root setting without evaluating YAML tags,
/// aliases, or Ruby. Ambiguous configuration requires an explicit CP_CACHE_DIR.
fn cocoapods_root(host: &dyn ProviderHost) -> Result<PathBuf, String> {
    if let Some(path) = host.variable("CP_CACHE_DIR") {
        return absolute_location(&path).ok_or("CP_CACHE_DIR is not absolute".into());
    }
    let home = host.home().ok_or("HOME unavailable")?;
    let config_home = host
        .variable("CP_HOME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cocoapods"));
    let config = config_home.join("config.yaml");
    let mut root = if host.variable("CP_HOME_DIR").is_some() {
        config_home.join("cache")
    } else {
        home.join("Library/Caches/CocoaPods")
    };
    safe_path(&config, Some(&home))?;
    match std::fs::metadata(&config) {
        Ok(metadata) if metadata.len() <= 64 * 1024 => {
            let text = std::fs::read_to_string(&config).map_err(|e| e.to_string())?;
            let mut found = false;
            for line in text.lines() {
                let Some(value) = line
                    .strip_prefix("cache_root:")
                    .or_else(|| line.strip_prefix(":cache_root:"))
                else {
                    continue;
                };
                if found {
                    return Err(
                        "duplicate CocoaPods cache_root; set CP_CACHE_DIR explicitly".into(),
                    );
                }
                found = true;
                let value = value.trim();
                let value = if value.starts_with('"') {
                    serde_json::from_str::<String>(value).map_err(
                        |_| "unsupported CocoaPods cache_root YAML; set CP_CACHE_DIR explicitly",
                    )?
                } else {
                    value.trim_matches('\'').to_owned()
                };
                if value.is_empty() || value.contains(['#', '&', '*', '!', '\n', '\'']) {
                    return Err(
                        "unsupported CocoaPods cache_root YAML; set CP_CACHE_DIR explicitly".into(),
                    );
                }
                root = value
                    .strip_prefix("~/")
                    .map_or_else(|| PathBuf::from(&value), |suffix| home.join(suffix));
            }
        }
        Ok(_) => {
            return Err(
                "CocoaPods user configuration exceeds 64 KiB; set CP_CACHE_DIR explicitly".into(),
            );
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    if !root.is_absolute() {
        return Err("CocoaPods cache_root is not absolute".into());
    }
    Ok(root)
}

/// Check every ancestor before walking: metadata inspection must not traverse
/// symlinked or dataless ancestors even for inventory-only provider state.
fn safe_path(path: &Path, home: Option<&Path>) -> Result<(), String> {
    if !path.is_absolute()
        || path.parent().is_none()
        || home.is_some_and(|home| home.starts_with(path))
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err("provider returned a non-absolute, root, home, or non-normal path".into());
    }
    if home.is_some_and(|home| {
        path.starts_with(home.join("Library/CloudStorage"))
            || path.starts_with(home.join("Library/Mobile Documents"))
    }) {
        return Err("provider storage is in synced cloud content".into());
    }
    let mut prefix = PathBuf::new();
    for part in path.components() {
        prefix.push(part);
        match NativeFileSystem.metadata(&prefix) {
            Ok(metadata)
                if metadata.kind == NodeKind::Symlink
                    || metadata.flags.contains(NodeFlags::DATALESS) =>
            {
                return Err(format!(
                    "symlinked/dataless provider ancestor: {}",
                    prefix.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// A measurement is cached within this inspection so overlapping alternative
/// actions reuse facts. The planning pipeline merges arenas for union accounting.
fn measure(
    item: &mut DetectedTarget,
    path: &Path,
    host: &dyn ProviderHost,
    scans: &mut Vec<ScanResult>,
    whole_cache: bool,
) {
    item.evidence
        .push(format!("storage location: {}", path.display()));
    if let Err(reason) = safe_path(path, host.home().as_deref()) {
        failed(item, reason);
        return;
    }
    if let Err(error) = std::fs::symlink_metadata(path) {
        if error.kind() == io::ErrorKind::NotFound {
            item.evidence
                .push("storage absent (not an inspection failure)".into());
            // Do not authorize a native cleaner to touch a newly created tree
            // which was absent from the reviewed measurement.
            if whole_cache {
                item.operation = PlannedOperation::Informational;
                item.usage = UsageEstimate::Unknown {
                    explanation: "storage absent; no cleanup needed".into(),
                };
            }
        } else {
            failed(item, error.to_string());
        }
        return;
    }
    if !scans.iter().any(|s| s.root == path) {
        match crate::system_targets::scan_storage_path(path) {
            Ok(scan) => scans.push(scan),
            Err(error) => {
                failed(item, format!("storage measurement failed: {error}"));
                return;
            }
        }
    }
    let scan = scans
        .iter()
        .find(|s| s.root == path)
        .expect("measured scan");
    let root = scan.arena.root();
    let metadata = &scan.arena.nodes[root.as_usize()];
    let safety = validate_scanned_candidate(&scan.arena, root, host.home().as_deref());
    if let CandidateSafety::Refused(reason) = safety {
        failed(item, reason);
    }
    if !scan.coverage_complete() || scan.mount_boundaries > 0 || scan.dataless_entries > 0 {
        failed(
            item,
            "provider storage has incomplete coverage, cloud content, or mount boundaries",
        );
    }
    if path
        .parent()
        .and_then(|p| NativeFileSystem.metadata(p).ok())
        .is_some_and(|p| p.device != metadata.device)
    {
        failed(item, "provider storage is a mount root");
    }
    let (apparent, allocated) = subtree_sizes(&scan.arena, root);
    let reclaimable = estimate_subtree(&scan.arena, root, scan.coverage_complete());
    item.evidence.push(format!(
        "occupied allocation: {allocated} bytes; this is not a native GC/prune yield"
    ));
    item.storage.push(StorageMeasurement {
        path: path.into(),
        apparent,
        allocated,
        reclaimable: reclaimable.clone(),
        identity: CandidateIdentity {
            device: metadata.device,
            inode: metadata.inode,
            kind: metadata.kind,
        },
    });
    if whole_cache {
        item.path = Some(path.into());
        item.usage = UsageEstimate::Filesystem {
            apparent,
            allocated,
            reclaimable,
        };
    }
}

fn children(path: &Path, host: &dyn ProviderHost) -> Result<Vec<PathBuf>, String> {
    safe_path(path, host.home().as_deref())?;
    let mut paths = Vec::new();
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(paths),
        Err(error) => return Err(error.to_string()),
    };
    for (count, entry) in entries.enumerate() {
        if count >= 1024 {
            return Err("provider inventory exceeded 1024 entries".into());
        }
        if paths.len() == 1024 {
            return Err("provider inventory exceeded 1024 directories".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn pin(item: &mut DetectedTarget, key: &str, value: &Path) {
    if let Some(spec) = command(item) {
        spec.environment
            .insert(key.into(), value.display().to_string());
    }
}

/// Inspect protection facts once per provider, not once per installation.
enum InstallationState {
    Conda {
        base: PathBuf,
        active: Option<PathBuf>,
    },
    Rustup {
        list: String,
        overrides: String,
    },
}

fn installation_state(
    item: &DetectedTarget,
    host: &dyn ProviderHost,
) -> Result<InstallationState, String> {
    let executable = item
        .provider
        .executable
        .as_deref()
        .ok_or("provider executable missing")?;
    match item.id.as_str() {
        "conda.environments" => {
            let value: serde_json::Value = serde_json::from_str(&query(host, executable, &["info", "--json"])?).map_err(|e| e.to_string())?;
            let base = value["root_prefix"].as_str().and_then(absolute_location).ok_or("Conda base environment unknown")?;
            let active = value.get("active_prefix").ok_or("active Conda environment unknown")?;
            let active = if active.is_null() { None } else {
                Some(active.as_str().and_then(absolute_location).ok_or("invalid active Conda environment")?)
            };
            Ok(InstallationState::Conda { base, active })
        }
        "rustup.toolchains" => Ok(InstallationState::Rustup {
            list: query(host, executable, &["toolchain", "list"])?,
            overrides: query(host, executable, &["override", "list"])?,
        }),
        _ => Err("Poetry removal requires a verified owning project; global environment path alone is insufficient".into()),
    }
}

/// A provider-wide ID must never authorize mass removal. Active/default/known
/// overridden installations remain protected even when manually ticked.
fn configure_individual(
    item: &mut DetectedTarget,
    path: &Path,
    host: &dyn ProviderHost,
    state: &Result<InstallationState, String>,
) {
    let Some(executable) = item.provider.executable.clone() else {
        return;
    };
    let state = match state {
        Ok(state) => state,
        Err(reason) => {
            item.evidence.push(format!("inventory only: {reason}"));
            return;
        }
    };
    let args = match state {
        InstallationState::Conda { base, active } => {
            if path == base
                || active.as_deref() == Some(path)
                || host
                    .variable("CONDA_PREFIX")
                    .is_some_and(|p| Path::new(&p) == path)
            {
                item.evidence
                    .push("inventory only: active/base Conda environment is protected".into());
                return;
            }
            vec![
                "env".into(),
                "remove".into(),
                "--prefix".into(),
                path.display().to_string(),
                "--yes".into(),
            ]
        }
        InstallationState::Rustup { list, overrides } => {
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                return;
            };
            let known = list
                .lines()
                .find(|line| line.split_whitespace().next() == Some(name));
            if known.is_none()
                || known.is_some_and(|line| line.contains("default") || line.contains("active"))
                || overrides
                    .lines()
                    .filter_map(|line| line.split_whitespace().last())
                    .any(|value| value == name || name.starts_with(&format!("{value}-")))
                || host
                    .variable("RUSTUP_TOOLCHAIN")
                    .is_some_and(|value| value == name || name.starts_with(&format!("{value}-")))
            {
                item.evidence.push("inventory only: default/active/overridden or unrecognized Rust toolchain is protected".into());
                return;
            }
            vec!["toolchain".into(), "uninstall".into(), name.into()]
        }
    };
    item.operation = PlannedOperation::Command {
        command: CommandSpec {
            executable,
            args,
            environment: plain_inspection_command(Path::new("/"), &[]).environment,
        },
    };
    item.consequence = "PERMANENT REMOVAL of this selected installation; projects may require it and manually installed state may be irrecoverable".into();
    if item.id == "rustup.toolchains" {
        if let Some(root) = home_path(host, "RUSTUP_HOME", ".rustup") {
            pin(item, "RUSTUP_HOME", &root);
        }
    }
}

/// Keep simulator user data individually reviewable. Passing the word
/// `unavailable` at execution would sweep devices not present in the review.
pub(super) fn simulators(
    host: &dyn ProviderHost,
    identity: &ProviderIdentity,
    executable: &Path,
) -> Vec<DetectedTarget> {
    let result = query(
        host,
        executable,
        &["simctl", "list", "devices", "unavailable", "--json"],
    )
    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).map_err(|e| e.to_string()));
    let mut base = provider_item(
        "xcode.simulators-unavailable",
        identity,
        "individually reviewed unavailable simulator",
        TargetClass::Irreplaceable,
        CleanupLevel::Standard,
        UsageEstimate::Unknown {
            explanation: "simulator device storage not measured".into(),
        },
        CandidateSafety::Eligible,
        PlannedOperation::Informational,
        None,
        0,
        "PERMANENT DATA LOSS: installed simulator apps/documents for this UUID are removed; a recreated simulator does not restore them",
        vec![],
    );
    let value = match result {
        Ok(value) => value,
        Err(reason) => {
            failed(&mut base, reason);
            return vec![base];
        }
    };
    let Some(groups) = value["devices"].as_object() else {
        failed(&mut base, "simctl JSON has no devices object");
        return vec![base];
    };
    let mut items = Vec::new();
    for device in groups
        .values()
        .filter_map(|v| v.as_array())
        .flatten()
        .take(129)
    {
        if items.len() >= 128 {
            failed(&mut base, "simulator inventory exceeds 128 devices");
            return vec![base];
        }
        let Some(uuid) = device["udid"].as_str().filter(|s| valid_uuid(s)) else {
            failed(&mut base, "simctl returned an invalid device UUID");
            return vec![base];
        };
        if device["isAvailable"].as_bool() != Some(false) {
            continue;
        }
        let mut item = base.clone();
        item.provider.context = Some(format!(
            "{}; unavailable simulator {uuid}",
            identity.context.as_deref().unwrap_or("Xcode")
        ));
        item.description = format!(
            "remove unavailable simulator {} ({uuid})",
            device["name"].as_str().unwrap_or("unnamed")
        );
        if let Some(home) = host.home() {
            item.path = Some(
                home.join("Library/Developer/CoreSimulator/Devices")
                    .join(uuid),
            );
            item.operation = PlannedOperation::Command {
                command: CommandSpec {
                    executable: executable.into(),
                    args: ["simctl", "delete", uuid].map(str::to_owned).to_vec(),
                    environment: BTreeMap::new(),
                },
            };
        }
        items.push(item);
    }
    items
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

pub(super) fn complete_inventory(
    inventory: &mut ProviderInventory,
    host: &dyn ProviderHost,
    _selected: &HashSet<String>,
) {
    // Hosts used for parser-only fixtures deliberately expose no filesystem
    // scope. Real inspection always supplies the user's explicit HOME.
    if host.home().is_none() {
        return;
    }
    let mut completed = Vec::new();
    for mut item in std::mem::take(&mut inventory.items) {
        if matches!(item.safety, CandidateSafety::Refused(_)) {
            if item.id == "bun.cache-clean" {
                if let Some(path) = home_path(host, "BUN_INSTALL_CACHE_DIR", ".bun/install/cache") {
                    measure(&mut item, &path, host, &mut inventory.scans, false);
                    item.operation = PlannedOperation::Informational;
                }
            }
            completed.push(item);
            continue;
        }
        let location = item.provider.context.as_deref().and_then(absolute_location);
        let executable = item.provider.executable.clone();
        let mut roots = Vec::new();
        let mut whole = false;
        match item.id.as_str() {
            "pnpm.store-prune" => {
                if let Some(path) = location {
                    // `store path` includes the schema version, whereas the
                    // --store-dir option takes its parent. Resolve the pinned
                    // configuration again rather than risking .../v10/v10.
                    let versioned = path.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
                        s.strip_prefix('v')
                            .is_some_and(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
                    });
                    let store = if versioned {
                        path.parent().expect("versioned store").to_path_buf()
                    } else {
                        path.clone()
                    };
                    let valid = executable.as_ref().is_some_and(|exe| {
                        query(
                            host,
                            exe,
                            &["store", "path", "--store-dir", &store.display().to_string()],
                        )
                        .ok()
                        .and_then(|s| absolute_location(&s))
                        .as_ref()
                            == Some(&path)
                    });
                    if valid {
                        if let Some(spec) = command(&mut item) {
                            spec.args
                                .extend(["--store-dir".into(), store.display().to_string()]);
                        }
                    } else {
                        failed(
                            &mut item,
                            "pnpm cannot resolve the reviewed store with an explicit --store-dir",
                        );
                    }
                    roots.push(path);
                }
                item.usage = UsageEstimate::Unknown {
                    explanation:
                        "cleanup yield unknown: pnpm prunes a subset of the measured store".into(),
                };
            }
            "npm.cache-verify" | "npm.npx-cache" => {
                if let Some(path) = location {
                    if let Some(spec) = command(&mut item) {
                        spec.args.push(format!("--cache={}", path.display()));
                    }
                    whole = item.id == "npm.npx-cache"
                        && !matches!(item.operation, PlannedOperation::Informational);
                    roots.push(path.join(if item.id == "npm.npx-cache" {
                        "_npx"
                    } else {
                        "_cacache"
                    }));
                }
                if !whole {
                    item.usage = UsageEstimate::Unknown {
                        explanation: if item.id == "npm.npx-cache" {
                            "inventory only: installed npm cache help does not advertise npx rm"
                                .into()
                        } else {
                            "cleanup yield unknown: npm verify only removes unneeded/corrupt content".into()
                        },
                    };
                }
            }
            "yarn.cache-clean" => {
                if let Some(path) = location {
                    if item.provider.version.as_deref().and_then(version_major) == Some(1) {
                        // Classic reports .../v6 but `cache clean` removes the
                        // entire cache root, including other schema versions.
                        let versioned =
                            path.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
                                s.strip_prefix('v').is_some_and(|v| {
                                    !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit())
                                })
                            });
                        if versioned {
                            let root = path
                                .parent()
                                .expect("versioned absolute cache")
                                .to_path_buf();
                            pin(&mut item, "YARN_CACHE_FOLDER", &root);
                            roots.push(root);
                        } else {
                            failed(
                                &mut item,
                                "unrecognized Yarn Classic versioned cache layout",
                            );
                        }
                    } else {
                        pin(&mut item, "YARN_GLOBAL_FOLDER", &path);
                        roots.push(path.join("cache"));
                    }
                    whole = true;
                }
            }
            "bun.cache-clean" => {
                if let Some(path) = location {
                    pin(&mut item, "BUN_INSTALL_CACHE_DIR", &path);
                    roots.push(path);
                    whole = true;
                }
            }
            "uv.cache-prune" | "uv.cache-clean" => {
                if let Some(path) = location {
                    pin(&mut item, "UV_CACHE_DIR", &path);
                    roots.push(path);
                }
                whole = item.id == "uv.cache-clean";
                if !whole {
                    item.usage = UsageEstimate::Unknown { explanation: "cleanup yield unknown: uv prune removes a subset of occupied cache storage".into() };
                }
            }
            "pip.cache-purge" => {
                if let Some(path) = location {
                    if let Some(spec) = command(&mut item) {
                        spec.args
                            .extend(["--cache-dir".into(), path.display().to_string()]);
                    }
                    roots.push(path);
                    // purge covers wheel/HTTP entries, not arbitrary files in
                    // the directory. Keep pip's parsed report separate from
                    // the occupied allocation of the whole configured root.
                    whole = false;
                }
            }
            "poetry.cache-clean" => {
                if let Some(path) = location {
                    pin(&mut item, "POETRY_CACHE_DIR", &path);
                    roots.push(path.join("cache/repositories/PyPI"));
                    whole = true;
                    item.evidence.push("scope: PyPI repository cache only; downloaded artifacts and other repositories are retained".into());
                }
            }
            "conda.cache-clean" => {
                // `conda info --json` supplies configured pkgs_dirs. Parse the
                // complete response, not the shortened display context.
                if let Some(exe) = &executable {
                    match query(host, exe, &["info", "--json"]).and_then(|text| {
                        serde_json::from_str::<serde_json::Value>(&text).map_err(|e| e.to_string())
                    }) {
                        Ok(value) => match json_paths(&value, "pkgs_dirs") {
                            Ok(paths) => roots.extend(paths),
                            Err(reason) => failed(&mut item, reason),
                        },
                        Err(reason) => failed(&mut item, reason),
                    }
                }
                item.usage = UsageEstimate::Unknown { explanation: "cleanup yield unknown: Conda dry-run reports tarball totals but no complete index/log total; extracted packages retained".into() };
            }
            "cargo.shared-cache" => {
                if let Some(root) = home_path(host, "CARGO_HOME", ".cargo") {
                    roots.extend([root.join("registry"), root.join("git")]);
                }
                item.usage = UsageEstimate::Unknown { explanation: "inventory only: installed Cargo has no validated granular stable cache-clean command; automatic GC is managed by Cargo".into() };
            }
            "rustup.toolchains" | "poetry.environments" | "conda.environments" => {
                let paths = match item.id.as_str() {
                    "rustup.toolchains" => home_path(host, "RUSTUP_HOME", ".rustup")
                        .map(|p| children(&p.join("toolchains"), host))
                        .unwrap_or_else(|| Ok(vec![])),
                    "poetry.environments" => {
                        location.map(|p| children(&p, host)).unwrap_or_else(|| {
                            Err("Poetry returned no absolute environments directory".into())
                        })
                    }
                    _ => executable
                        .as_ref()
                        .ok_or_else(|| "Conda executable missing".into())
                        .and_then(|exe| query(host, exe, &["env", "list", "--json"]))
                        .and_then(|text| {
                            serde_json::from_str::<serde_json::Value>(&text)
                                .map_err(|e| e.to_string())
                        })
                        .and_then(|v| json_paths(&v, "envs")),
                };
                match paths {
                    Ok(paths) if paths.len() > 128 => failed(
                        &mut item,
                        "individual installation inventory exceeds 128 entries",
                    ),
                    Ok(paths) if !paths.is_empty() => {
                        let state = installation_state(&item, host);
                        for path in paths {
                            let mut individual = item.clone();
                            individual.provider.context =
                                Some(format!("{}: {}", item.provider.id, path.display()));
                            individual.operation = PlannedOperation::Informational;
                            individual.path = Some(path.clone());
                            individual.evidence.push("project dependencies outside inspected roots are unknown; manually installed packages/components may be lost".into());
                            configure_individual(&mut individual, &path, host, &state);
                            let removable =
                                !matches!(individual.operation, PlannedOperation::Informational);
                            measure(
                                &mut individual,
                                &path,
                                host,
                                &mut inventory.scans,
                                removable,
                            );
                            completed.push(individual);
                        }
                        continue;
                    }
                    Ok(_) => item
                        .evidence
                        .push("no individual installations found".into()),
                    Err(reason) => failed(&mut item, reason),
                }
            }
            "cocoapods.cache-clean" => match cocoapods_root(host) {
                Ok(path) => {
                    pin(&mut item, "CP_CACHE_DIR", &path);
                    roots.push(path.join("Pods"));
                    whole = true;
                }
                Err(reason) => failed(&mut item, reason),
            },
            "homebrew.cleanup" | "homebrew.cleanup.scrub" => {
                item.evidence.push("scope: native cleanup/scrub covers stale downloads AND old installed versions; a separate full-download-cache deletion is not validated".into());
                if let Some(path) = item.evidence.iter().find_map(|s| {
                    s.strip_prefix("configured cache: ")
                        .and_then(absolute_location)
                }) {
                    pin(&mut item, "HOMEBREW_CACHE", &path);
                    roots.push(path);
                }
            }
            "homebrew.autoremove" => {
                if let Some(exe) = &executable {
                    match query(host, exe, &["autoremove", "--dry-run"]) {
                        Ok(text) => {
                            let formulae: Vec<_> = text
                                .lines()
                                .skip(1)
                                .map(str::trim)
                                .filter(|s| {
                                    !s.is_empty()
                                        && s.bytes().all(|c| {
                                            c.is_ascii_alphanumeric()
                                                || matches!(
                                                    c,
                                                    b'@' | b'+' | b'-' | b'.' | b'_' | b'/'
                                                )
                                        })
                                })
                                .collect();
                            for formula in formulae.iter().take(128) {
                                match query(host, exe, &["--cellar", formula])
                                    .ok()
                                    .and_then(|s| absolute_location(&s))
                                {
                                    Some(path) => roots.push(path),
                                    None => item.evidence.push(format!(
                                        "removable formula {formula}: location not measured"
                                    )),
                                }
                            }
                            if formulae.len() > 128 {
                                item.evidence.push(
                                    "Homebrew removable-object measurement limited to 128 formulae"
                                        .into(),
                                );
                            }
                        }
                        Err(reason) => failed(&mut item, reason),
                    }
                }
            }
            "nix.gc" | "nix.generations" => match host.resolve_executable("nix-store") {
                Ok(Some(exe)) => match query(host, &exe, &["--gc", "--print-dead"]) {
                    Ok(text) => {
                        let paths: Vec<_> = text.lines().filter_map(absolute_location).collect();
                        item.evidence.push(format!("{} currently unreachable Nix store paths; generation deletion can make more paths unreachable", paths.len()));
                        if paths.len() > 128 {
                            item.evidence.push(
                                "Nix object measurement limited to the first 128 roots".into(),
                            );
                        }
                        roots.extend(paths.into_iter().take(128));
                        item.usage = UsageEstimate::Unknown { explanation: "cleanup yield unknown: measured unreachable objects are inventory; live GC roots, sharing and generation deletion can change the set".into() };
                    }
                    Err(reason) => item
                        .evidence
                        .push(format!("removable-object inventory unavailable: {reason}")),
                },
                Ok(None) => item.evidence.push(
                    "removable-object sizing unavailable: nix-store executable not found".into(),
                ),
                Err(error) => item.evidence.push(error.to_string()),
            },
            id if id.starts_with("xcode.") && id != "xcode.simulators-unavailable" => {
                if let PlannedOperation::DirectDelete { path, .. } = &item.operation {
                    roots.push(path.clone());
                } else if let Some(path) = item.evidence.iter().find_map(|s| {
                    s.strip_prefix("detected known Xcode path: ")
                        .and_then(absolute_location)
                }) {
                    roots.push(path);
                }
                whole = !matches!(item.operation, PlannedOperation::Informational);
            }
            "xcode.simulators-unavailable" => {
                if let Some(path) = &item.path {
                    roots.push(path.clone());
                    whole = true;
                }
            }
            "orbstack.machines" => {
                item.usage = UsageEstimate::Unknown { explanation: "inventory only: OrbStack machine allocation cannot be separated safely from its shared backing storage; native per-machine size/removal needs macOS validation".into() };
            }
            _ => {}
        }
        roots.sort();
        roots.dedup();
        // Reject missing location output for cleaners which otherwise could
        // mutate an entirely uninspected, configuration-selected directory.
        if roots.is_empty()
            && matches!(
                item.id.as_str(),
                "pnpm.store-prune"
                    | "npm.cache-verify"
                    | "npm.npx-cache"
                    | "yarn.cache-clean"
                    | "bun.cache-clean"
                    | "uv.cache-prune"
                    | "uv.cache-clean"
                    | "pip.cache-purge"
                    | "poetry.cache-clean"
                    | "conda.cache-clean"
                    | "cocoapods.cache-clean"
            )
        {
            failed(
                &mut item,
                "provider did not return an absolute supported cache location",
            );
        }
        for root in roots {
            measure(&mut item, &root, host, &mut inventory.scans, whole);
        }
        completed.push(item);
    }
    inventory.items = completed;
}

/// Revalidate every measured native-command root, including partial-prune
/// storage whose occupied allocation deliberately is not a cleanup estimate.
pub fn revalidate_storage(target: &DetectedTarget, home: Option<&Path>) -> Result<(), String> {
    for storage in &target.storage {
        safe_path(&storage.path, home)?;
        let scan =
            crate::system_targets::scan_storage_path(&storage.path).map_err(|e| e.to_string())?;
        let root = &scan.arena.nodes[scan.arena.root().as_usize()];
        let current = CandidateIdentity {
            device: root.device,
            inode: root.inode,
            kind: root.kind,
        };
        if current != storage.identity
            || !scan.coverage_complete()
            || scan.mount_boundaries > 0
            || scan.dataless_entries > 0
        {
            return Err(
                "provider storage identity/coverage/cloud state changed after review".into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
