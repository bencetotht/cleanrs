//! Provider-owned system storage. Discovery is limited to explicit tool paths
//! and known cache roots; it never recursively searches the user's home.
//! Protected VM/application state is inventory only, not a deletion plan.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::fs::{Arena, FileSystem, NativeFileSystem, Node, NodeFlags, NodeKind, ScanResult};
use crate::providers::{ProviderHost, ProviderInventory, ProviderStatus};
use crate::targets::{
    Automation, CandidateIdentity, CandidateSafety, CleanupLevel, CommandSpec, DetectedTarget,
    PlannedOperation, ProviderIdentity, TargetClass, UsageEstimate, modified_age_days,
    subtree_sizes, validate_scanned_candidate,
};

pub const ACTION_IDS: &[&str] = &[
    "gradle.caches",
    "gradle.wrapper",
    "gradle.daemon-logs",
    "android.system-images",
    "android.build-tools",
    "android.avds",
    "metro.cache",
    "expo.global-cache",
    "watchman.watches",
    "go.build-cache",
    "go.module-cache",
    "maven.repository",
    "playwright.browsers",
    "puppeteer.browsers",
    "node-gyp.headers",
    "swiftpm.repositories",
    "typescript.cache",
    "vmware.bundles",
    "claude.cowork",
    "wallpaper.cache",
];

#[derive(Debug, Clone)]
pub struct SystemContext {
    pub home: Option<PathBuf>,
    pub variables: BTreeMap<String, String>,
    pub macos: bool,
}
impl SystemContext {
    pub fn from_environment(home: Option<&Path>) -> Self {
        let variables = [
            "GRADLE_USER_HOME",
            "ANDROID_HOME",
            "ANDROID_SDK_ROOT",
            "ANDROID_AVD_HOME",
            "TMPDIR",
            "PUPPETEER_CACHE_DIR",
            "PLAYWRIGHT_BROWSERS_PATH",
            "XDG_CACHE_HOME",
            "npm_config_devdir",
        ]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (name.to_owned(), value))
        })
        .collect();
        Self {
            home: home.map(Path::to_path_buf),
            variables,
            macos: cfg!(target_os = "macos"),
        }
    }
    fn path(&self, variable: &str, fallback: PathBuf) -> PathBuf {
        self.variables
            .get(variable)
            .map(PathBuf::from)
            .unwrap_or(fallback)
    }
    fn cache(&self, home: &Path) -> PathBuf {
        if self.macos {
            home.join("Library/Caches")
        } else {
            self.path("XDG_CACHE_HOME", home.join(".cache"))
        }
    }
}

fn command(executable: PathBuf, args: &[&str]) -> CommandSpec {
    CommandSpec {
        executable,
        args: args.iter().map(|s| (*s).to_owned()).collect(),
        environment: BTreeMap::new(),
    }
}
fn stdout(host: &dyn ProviderHost, spec: &CommandSpec) -> Option<String> {
    host.run_inspection(spec)
        .ok()
        .filter(|output| output.exit_code == Some(0))
        .map(|output| output.stdout)
}
fn identity(
    id: &str,
    path: &Path,
    executable: Option<PathBuf>,
    version: Option<String>,
) -> ProviderIdentity {
    ProviderIdentity {
        id: format!("storage.{id}"),
        executable,
        version,
        context: Some(path.display().to_string()),
    }
}
fn no_symlinks(path: &Path) -> bool {
    let mut prefix = PathBuf::new();
    for part in path.components() {
        prefix.push(part);
        if !NativeFileSystem.metadata(&prefix).is_ok_and(|metadata| {
            metadata.kind != NodeKind::Symlink && !metadata.flags.contains(NodeFlags::DATALESS)
        }) {
            return false;
        }
    }
    true
}

/// Measure a provider-owned file without scanning its (possibly shared) parent.
/// Directories retain the scanner's coverage and dataless checks.
pub fn scan_storage_path(path: &Path) -> std::io::Result<ScanResult> {
    let metadata = NativeFileSystem.metadata(path)?;
    if metadata.kind != NodeKind::File {
        return crate::Scanner::new(Default::default()).scan(path);
    }
    let mut arena = Arena::new(path);
    let node = &mut arena.nodes[0];
    *node = Node::default();
    node.kind = metadata.kind;
    node.apparent_size = metadata.apparent_size;
    node.allocated_size = metadata.allocated_size;
    node.private_size = metadata.private_size;
    node.device = metadata.device;
    node.inode = metadata.inode;
    node.link_count = metadata.link_count;
    node.clone_id = metadata.clone_id;
    node.clone_refcount = metadata.clone_refcount;
    node.modified_unix_seconds = metadata.modified_unix_seconds;
    node.flags = metadata.flags;
    Ok(ScanResult {
        root: path.into(),
        arena,
        coverage_errors: vec![],
        backend: "provider file metadata".into(),
        dataless_entries: usize::from(metadata.flags.contains(NodeFlags::DATALESS)),
        mount_boundaries: 0,
    })
}

/// Inspect one exact provider-owned directory. The measured arena is retained
/// for union accounting, so private sizes or overlapping paths are never summed.
#[allow(clippy::too_many_arguments)]
fn directory(
    inventory: &mut ProviderInventory,
    context: &SystemContext,
    id: &str,
    path: PathBuf,
    class: TargetClass,
    level: CleanupLevel,
    days: u32,
    consequence: &str,
    provider: ProviderIdentity,
    operation: Option<CommandSpec>,
    informational: bool,
) {
    if std::fs::symlink_metadata(&path).is_err() {
        return;
    }
    let mut safety;
    let forbidden = !path.is_absolute()
        || path == Path::new("/")
        || context.home.as_ref() == Some(&path)
        || !no_symlinks(&path)
        || context.home.as_ref().is_some_and(|home| {
            path.starts_with(home.join("Library/CloudStorage"))
                || path.starts_with(home.join("Library/Mobile Documents"))
        });
    // Normalize only after rejecting symlinks and non-normal components.
    let forbidden = forbidden
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        });
    let scan = if forbidden {
        None
    } else {
        scan_storage_path(&path).ok()
    };
    let mut usage = UsageEstimate::Unknown {
        explanation: "provider path could not be safely measured".into(),
    };
    let mut age = None;
    let mut planned = PlannedOperation::Informational;
    let mut evidence =
        vec!["system scope: exact provider-owned storage; no home-directory search".into()];
    if let Some(scan) = scan {
        let root = scan.arena.root();
        safety = validate_scanned_candidate(&scan.arena, root, context.home.as_deref());
        if !scan.coverage_complete() {
            safety = CandidateSafety::Refused("provider cache coverage is incomplete".into());
        }
        let metadata = &scan.arena.nodes[root.as_usize()];
        if let Some(parent) = path
            .parent()
            .and_then(|p| NativeFileSystem.metadata(p).ok())
        {
            if parent.device != metadata.device {
                safety = CandidateSafety::Refused("provider cache is a mount root".into());
            }
        }
        let (apparent, allocated) = subtree_sizes(&scan.arena, root);
        usage = UsageEstimate::Filesystem {
            apparent,
            allocated,
            reclaimable: crate::accounting::estimate_subtree(
                &scan.arena,
                root,
                scan.coverage_complete(),
            ),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        age = modified_age_days(&scan.arena, root, now);
        if !informational {
            planned = operation.map_or_else(
                || PlannedOperation::DirectDelete {
                    path: scan.root.clone(),
                    children_only: metadata.kind == NodeKind::Directory,
                    identity: CandidateIdentity {
                        device: metadata.device,
                        inode: metadata.inode,
                        kind: metadata.kind,
                    },
                },
                |command| PlannedOperation::Command { command },
            );
        }
        if id == "vmware.bundles" {
            for suffix in ["vmdk", "vmsn", "vmem", "vmss", "lck"] {
                let count = (0..scan.arena.nodes.len())
                    .filter(|index| {
                        scan.arena
                            .path(crate::fs::NodeIndex::from_usize(*index))
                            .extension()
                            .is_some_and(|extension| extension == suffix)
                    })
                    .count();
                if count > 0 {
                    evidence.push(format!(
                        "VMware .{suffix} components: {count}; never removed individually"
                    ));
                }
            }
        }
        // Maven's repository can also contain unpublished local installs.
        // A network download cannot restore those; preserve the entire mixed
        // repository rather than treating it as a disposable cache.
        if id == "maven.repository"
            && scan.arena.nodes.iter().enumerate().any(|(index, _)| {
                scan.arena.name(crate::fs::NodeIndex::from_usize(index))
                    == "maven-metadata-local.xml"
            })
        {
            safety = CandidateSafety::Refused("Maven repository contains locally installed artifacts; preserve unpublished packages".into());
            planned = PlannedOperation::Informational;
        }
        inventory.scans.push(scan);
    } else {
        safety = CandidateSafety::Refused("provider path is protected, symlinked, inaccessible, or outside a supported local cache".into());
    }
    let mut properties = class.properties();
    if !informational && properties.automation != Automation::ExplicitTargetOnly {
        properties.automation = Automation::Confirmed;
    }
    inventory.items.push(DetectedTarget {
        gitignore: None,
        cargo_ownership: None,
        id: id.into(),
        provider,
        description: consequence.into(),
        project_root: None,
        path: Some(path),
        class,
        properties,
        minimum_level: level,
        usage,
        age_days: age,
        stale_after_days: days,
        stale: if days == 0 {
            Some(true)
        } else {
            age.map(|age| age >= u64::from(days))
        },
        safety,
        operation: planned,
        exclusive_group: None,
        exclusive_priority: 0,
        consequence: consequence.into(),
        evidence,
    });
}

/// Metadata-only, bounded enumeration; never open a placeholder or follow links.
fn local_directory(path: &Path) -> bool {
    no_symlinks(path)
        && NativeFileSystem.metadata(path).is_ok_and(|metadata| {
            metadata.kind == NodeKind::Directory
                && !metadata
                    .flags
                    .intersects(NodeFlags::DATALESS | NodeFlags::MOUNT_BOUNDARY)
        })
}
fn child_directories(root: &Path, limit: usize) -> Vec<PathBuf> {
    if !local_directory(root) {
        return Vec::new();
    }
    let mut entries = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .take(limit)
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

/// The user's remembered extension identifier is a discovery hint, not a
/// validated generated-cache schema. Bound both depth and entries, and never
/// classify its enclosing container (which also holds wallpaper sources).
fn wallpaper_directories(container: &Path) -> Vec<PathBuf> {
    let mut pending = vec![(container.to_path_buf(), 0)];
    let mut found = Vec::new();
    let mut budget = 4096usize;
    while let Some((path, depth)) = pending.pop() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        if !local_directory(&path) {
            continue;
        }
        if path
            .file_name()
            .is_some_and(|name| name == "extension-com.apple.wallpaper.extension.image")
        {
            found.push(path);
            continue;
        }
        if depth < 10 {
            pending.extend(
                child_directories(&path, budget.min(256))
                    .into_iter()
                    .map(|p| (p, depth + 1)),
            );
        }
    }
    found.sort();
    found
}

pub fn inspect(
    host: &dyn ProviderHost,
    context: &SystemContext,
    selected: &HashSet<String>,
    mut progress: impl FnMut(&str),
) -> ProviderInventory {
    let mut inventory = ProviderInventory::default();
    let Some(home) = &context.home else {
        return inventory;
    };
    let cache = context.cache(home);
    for provider in [
        "gradle",
        "android",
        "metro",
        "expo",
        "watchman",
        "go",
        "maven",
        "playwright",
        "puppeteer",
        "node-gyp",
        "swiftpm",
        "typescript",
        "vmware",
        "claude",
        "wallpaper",
    ] {
        let ids: Vec<_> = ACTION_IDS
            .iter()
            .copied()
            .filter(|id| id.split('.').next() == Some(provider) && selected.contains(*id))
            .collect();
        if ids.is_empty() {
            continue;
        }
        progress(provider);
        let before = inventory.items.len();
        let wanted = |id: &str| ids.contains(&id);
        match provider {
            "gradle" => {
                let root = context.path("GRADLE_USER_HOME", home.join(".gradle"));
                let executable = host.resolve_executable("gradle").ok().flatten();
                let version = executable
                    .as_ref()
                    .and_then(|p| stdout(host, &command(p.clone(), &["--version"])));
                let stopped = executable
                    .as_ref()
                    .and_then(|p| stdout(host, &command(p.clone(), &["--offline", "--status"])))
                    .is_some_and(|text| text.contains("No Gradle daemons are running"));
                for (id, relative, class, consequence) in [
                    (
                        "gradle.caches",
                        "caches",
                        TargetClass::Network,
                        "Gradle dependencies and transforms must be downloaded/rebuilt; stop Gradle before cleanup",
                    ),
                    (
                        "gradle.wrapper",
                        "wrapper/dists",
                        TargetClass::Network,
                        "Gradle wrapper distributions must be downloaded again; stop Gradle before cleanup",
                    ),
                    (
                        "gradle.daemon-logs",
                        "daemon",
                        TargetClass::Cheap,
                        "Stopped Gradle daemon logs/registry state will be regenerated",
                    ),
                ] {
                    if !wanted(id) {
                        continue;
                    }
                    let path = root.join(relative);
                    let identity = identity(provider, &root, executable.clone(), version.clone());
                    directory(
                        &mut inventory,
                        context,
                        id,
                        path,
                        class,
                        CleanupLevel::Standard,
                        30,
                        consequence,
                        identity,
                        None,
                        !stopped,
                    );
                    if !stopped {
                        if let Some(item) = inventory.items.last_mut().filter(|i| i.id == id) {
                            item.evidence.push(
                                "Gradle daemon quiescence could not be proven; inspection only"
                                    .into(),
                            );
                        }
                    }
                }
            }
            "android" => inspect_android(&mut inventory, host, context, &ids),
            "metro" => {
                let root = context.path("TMPDIR", std::env::temp_dir());
                // Only top-level Metro/haste names are recognized. Never offer
                // TMPDIR itself, arbitrary app state, or symlink destinations.
                if !local_directory(&root) {
                    continue;
                }
                for entry in std::fs::read_dir(&root)
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .take(4096)
                {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if (name.starts_with("metro-") || name.starts_with("haste-map-"))
                        && entry.file_type().is_ok_and(|t| t.is_dir() || t.is_file())
                    {
                        let path = entry.path();
                        directory(
                            &mut inventory,
                            context,
                            "metro.cache",
                            path.clone(),
                            TargetClass::Cheap,
                            CleanupLevel::Standard,
                            14,
                            "Metro/haste-map generated caches will be rebuilt",
                            identity(provider, &path, None, None),
                            None,
                            false,
                        );
                    }
                }
            }
            "watchman" => {
                if let Some(executable) = host.resolve_executable("watchman").ok().flatten() {
                    let version = stdout(host, &command(executable.clone(), &["--version"]));
                    if let Some(watches) =
                        stdout(host, &command(executable.clone(), &["watch-list"]))
                    {
                        let valid = serde_json::from_str::<serde_json::Value>(&watches)
                            .ok()
                            .is_some_and(|v| v["roots"].is_array() && v.get("error").is_none());
                        if valid {
                            let mut properties = TargetClass::Cheap.properties();
                            properties.automation = Automation::Confirmed;
                            inventory.items.push(DetectedTarget { gitignore: None, cargo_ownership: None, id: "watchman.watches".into(),
                                provider: identity(provider, Path::new("watchman"), Some(executable.clone()), version), description: "Reset Watchman watches".into(), project_root: None, path: None,
                                class: TargetClass::Cheap, properties, minimum_level: CleanupLevel::Standard, usage: UsageEstimate::Unknown { explanation: "Watchman watch state is not a measured disk-reclamation estimate".into() }, age_days: None, stale_after_days: 0, stale: Some(true), safety: CandidateSafety::Eligible,
                                operation: PlannedOperation::Command { command: command(executable, &["watch-del-all"]) }, exclusive_group: None, exclusive_priority: 0,
                                consequence: "All Watchman watches are reset; clients must recreate them".into(), evidence: vec![watches] });
                        }
                    }
                }
            }
            "go" => inspect_go(&mut inventory, host, context, &ids),
            "swiftpm" => {
                let root = cache.join("org.swift.swiftpm");
                let executable = host.resolve_executable("swift").ok().flatten();
                let version = executable
                    .as_ref()
                    .and_then(|p| stdout(host, &command(p.clone(), &["--version"])));
                let native = executable
                    .as_ref()
                    .filter(|p| {
                        stdout(host, &command((*p).clone(), &["package", "--help"]))
                            .is_some_and(|help| help.contains("purge-cache"))
                    })
                    .map(|p| {
                        command(
                            p.clone(),
                            &[
                                "package",
                                "--cache-path",
                                &root.to_string_lossy(),
                                "purge-cache",
                            ],
                        )
                    });
                directory(
                    &mut inventory,
                    context,
                    "swiftpm.repositories",
                    root.join("repositories"),
                    TargetClass::Network,
                    CleanupLevel::Standard,
                    30,
                    "SwiftPM repository cache will need to be downloaded again",
                    identity(provider, &root, executable, version),
                    native.clone(),
                    native.is_none(),
                );
            }
            "vmware" => {
                for parent in [
                    home.join("Virtual Machines.localized"),
                    home.join("Virtual Machines"),
                    home.join("Documents/Virtual Machines.localized"),
                ] {
                    for path in child_directories(&parent, 128)
                        .into_iter()
                        .filter(|p| p.extension().is_some_and(|e| e == "vmwarevm"))
                    {
                        directory(
                            &mut inventory,
                            context,
                            "vmware.bundles",
                            path.clone(),
                            TargetClass::Irreplaceable,
                            CleanupLevel::Aggressive,
                            0,
                            "VMware bundle is user data: disks, snapshots and suspended sessions; use Fusion's own management tools",
                            identity(provider, &path, None, None),
                            None,
                            true,
                        );
                    }
                }
            }
            "claude" => {
                let path =
                    home.join("Library/Application Support/Claude/vm_bundles/claudevm.bundle");
                directory(
                    &mut inventory,
                    context,
                    "claude.cowork",
                    path.clone(),
                    TargetClass::Irreplaceable,
                    CleanupLevel::Aggressive,
                    0,
                    "Cowork VM/session state may be persistent; no validated reset API, inventory only",
                    identity(provider, &path, None, None),
                    None,
                    true,
                );
            }
            "wallpaper" => {
                let version = host
                    .resolve_executable("sw_vers")
                    .ok()
                    .flatten()
                    .and_then(|p| stdout(host, &command(p, &["-productVersion"])));
                if context.macos {
                    let container = home.join("Library/Containers/com.apple.wallpaper.agent");
                    for path in wallpaper_directories(&container) {
                        directory(
                            &mut inventory,
                            context,
                            "wallpaper.cache",
                            path.clone(),
                            TargetClass::Irreplaceable,
                            CleanupLevel::Aggressive,
                            0,
                            "Wallpaper extension storage is unverified: macOS version, generated-cache layout, and agent quiescence must be validated before cleanup; source images are protected",
                            identity(provider, &path, None, version.clone()),
                            None,
                            true,
                        );
                    }
                }
            }
            _ => {
                let (id, path, class, days, consequence) = match provider {
                    "expo" => (
                        "expo.global-cache",
                        home.join(".expo/cache"),
                        TargetClass::Network,
                        30,
                        "Expo downloaded cache must be restored; credentials and .expo settings are retained",
                    ),
                    "maven" => (
                        "maven.repository",
                        home.join(".m2/repository"),
                        TargetClass::Network,
                        30,
                        "Maven dependencies must be downloaded again; locally installed unpublished artifacts may be lost",
                    ),
                    "playwright" => (
                        "playwright.browsers",
                        context.path("PLAYWRIGHT_BROWSERS_PATH", cache.join("ms-playwright")),
                        TargetClass::Expensive,
                        30,
                        "Playwright browsers must be downloaded again; tests using them cannot run until restored",
                    ),
                    "puppeteer" => (
                        "puppeteer.browsers",
                        context.path("PUPPETEER_CACHE_DIR", home.join(".cache/puppeteer")),
                        TargetClass::Expensive,
                        30,
                        "Puppeteer browser downloads must be restored before automation can run",
                    ),
                    "node-gyp" => (
                        "node-gyp.headers",
                        context.path("npm_config_devdir", cache.join("node-gyp")),
                        TargetClass::Network,
                        30,
                        "Node-gyp headers must be downloaded for subsequent native builds",
                    ),
                    "typescript" => (
                        "typescript.cache",
                        cache.join("typescript"),
                        TargetClass::Network,
                        30,
                        "TypeScript downloaded typings cache must be restored",
                    ),
                    _ => continue,
                };
                let level = if class == TargetClass::Expensive {
                    CleanupLevel::Aggressive
                } else {
                    CleanupLevel::Standard
                };
                directory(
                    &mut inventory,
                    context,
                    id,
                    path.clone(),
                    class,
                    level,
                    days,
                    consequence,
                    identity(provider, &path, None, None),
                    None,
                    false,
                );
            }
        }
        inventory.statuses.push(ProviderStatus {
            id: provider.into(),
            detected: inventory.items.len() > before,
            version: None,
            context: None,
            message: format!(
                "{} system action(s) inspected",
                inventory.items.len() - before
            ),
        });
    }
    inventory
}

fn inspect_go(
    inventory: &mut ProviderInventory,
    host: &dyn ProviderHost,
    context: &SystemContext,
    ids: &[&str],
) {
    let Some(executable) = host.resolve_executable("go").ok().flatten() else {
        return;
    };
    let mut spec = command(
        executable.clone(),
        &["env", "-json", "GOCACHE", "GOMODCACHE"],
    );
    spec.environment.extend([
        ("GOTOOLCHAIN".into(), "local".into()),
        ("GOPROXY".into(), "off".into()),
        ("GOWORK".into(), "off".into()),
    ]);
    let Some(value) =
        stdout(host, &spec).and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    else {
        return;
    };
    let mut version_spec = command(executable.clone(), &["version"]);
    version_spec.environment = spec.environment.clone();
    let version = stdout(host, &version_spec);
    for (id, key, flag, class, level) in [
        (
            "go.build-cache",
            "GOCACHE",
            "-cache",
            TargetClass::Cheap,
            CleanupLevel::Safe,
        ),
        (
            "go.module-cache",
            "GOMODCACHE",
            "-modcache",
            TargetClass::Network,
            CleanupLevel::Standard,
        ),
    ] {
        if !ids.contains(&id) {
            continue;
        }
        let Some(path) = value[key]
            .as_str()
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            continue;
        };
        let mut native = command(executable.clone(), &["clean", flag]);
        native.environment = spec.environment.clone();
        native
            .environment
            .insert(key.into(), path.display().to_string());
        directory(
            inventory,
            context,
            id,
            path.clone(),
            class,
            level,
            0,
            if key == "GOCACHE" {
                "Go build cache will be rebuilt"
            } else {
                "Go modules must be downloaded again; local edits to cached dependencies may be lost"
            },
            identity("go", &path, Some(executable.clone()), version.clone()),
            Some(native),
            false,
        );
        if key == "GOCACHE" {
            if let Some(item) = inventory.items.last_mut().filter(|i| i.id == id) {
                item.properties.automation = Automation::Unattended;
            }
        }
    }
}

fn inspect_android(
    inventory: &mut ProviderInventory,
    host: &dyn ProviderHost,
    context: &SystemContext,
    ids: &[&str],
) {
    let Some(home) = &context.home else {
        return;
    };
    let root = context.path(
        "ANDROID_SDK_ROOT",
        context.path("ANDROID_HOME", home.join("Library/Android/sdk")),
    );
    let executable = host
        .resolve_executable("sdkmanager")
        .ok()
        .flatten()
        .or_else(|| {
            let path = root.join("cmdline-tools/latest/bin/sdkmanager");
            path.is_file().then_some(path)
        });
    let version = executable
        .as_ref()
        .and_then(|p| stdout(host, &command(p.clone(), &["--version"])));
    // Directory names alone are not installation evidence. Pin inventory and
    // uninstallation to the same SDK, and offer only listed package IDs.
    let installed = executable
        .as_ref()
        .and_then(|p| {
            stdout(
                host,
                &command(
                    p.clone(),
                    &[
                        &format!("--sdk_root={}", root.display()),
                        "--list_installed",
                    ],
                ),
            )
        })
        .map(|output| {
            output
                .lines()
                .filter_map(|line| line.split('|').next())
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect::<HashSet<_>>()
        });
    for (id, relative, depth) in [
        ("android.system-images", "system-images", 3),
        ("android.build-tools", "build-tools", 1),
    ] {
        if !ids.contains(&id) {
            continue;
        }
        let mut paths = vec![root.join(relative)];
        for _ in 0..depth {
            paths = paths
                .iter()
                .flat_map(|p| child_directories(p, 256))
                .take(1024)
                .collect();
        }
        for path in paths {
            let Some(package) = path.strip_prefix(&root).ok().map(|p| {
                p.iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(";")
            }) else {
                continue;
            };
            if !package
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-;".contains(c))
            {
                continue;
            }
            let native = executable
                .as_ref()
                .filter(|_| {
                    version.is_some()
                        && installed
                            .as_ref()
                            .is_some_and(|packages| packages.contains(&package))
                })
                .map(|p| {
                    command(
                        p.clone(),
                        &[
                            &format!("--sdk_root={}", root.display()),
                            "--uninstall",
                            &package,
                        ],
                    )
                });
            directory(
                inventory,
                context,
                id,
                path.clone(),
                TargetClass::Expensive,
                CleanupLevel::Aggressive,
                30,
                "Android SDK package will be uninstalled; affected builds/emulators require downloading it again",
                identity("android", &root, executable.clone(), version.clone()),
                native.clone(),
                native.is_none(),
            );
            if let Some(item) = inventory
                .items
                .last_mut()
                .filter(|i| i.path.as_ref() == Some(&path))
            {
                item.evidence.push(format!("SDK package: {package}"));
            }
        }
    }
    if ids.contains(&"android.avds") {
        let root = context.path("ANDROID_AVD_HOME", home.join(".android/avd"));
        for path in child_directories(&root, 256)
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "avd"))
        {
            directory(
                inventory,
                context,
                "android.avds",
                path.clone(),
                TargetClass::Irreplaceable,
                CleanupLevel::Aggressive,
                0,
                "Android emulator disk and application state are user data; inventory only",
                identity("android", &path, None, None),
                None,
                true,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{CommandOutput, ProviderError};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Host {
        outputs: BTreeMap<String, String>,
        commands: Mutex<Vec<CommandSpec>>,
    }
    impl ProviderHost for Host {
        fn resolve_executable(&self, name: &str) -> Result<Option<PathBuf>, ProviderError> {
            Ok(self
                .outputs
                .contains_key(name)
                .then(|| PathBuf::from(format!("/fixture/{name}"))))
        }
        fn run_inspection(&self, spec: &CommandSpec) -> Result<CommandOutput, ProviderError> {
            self.commands.lock().unwrap().push(spec.clone());
            let name = spec.executable.file_name().unwrap().to_str().unwrap();
            let output = self
                .outputs
                .get(&format!("{name} {}", spec.args.join(" ")))
                .or_else(|| self.outputs.get(name))
                .cloned()
                .unwrap_or_default();
            Ok(CommandOutput {
                exit_code: Some(0),
                stdout: output,
                stderr: String::new(),
            })
        }
    }
    fn context(home: &Path) -> SystemContext {
        SystemContext {
            home: Some(home.into()),
            variables: BTreeMap::new(),
            macos: true,
        }
    }
    fn cache(home: &Path, relative: &str) -> PathBuf {
        let path = home.join(relative);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("fixture"), b"generated").unwrap();
        path
    }
    fn inspect_ids(host: &Host, context: &SystemContext, ids: &[&str]) -> ProviderInventory {
        inspect(
            host,
            context,
            &ids.iter().map(|s| (*s).into()).collect(),
            |_| {},
        )
    }
    #[test]
    fn known_caches_are_separate_confirmed_actions_and_preserve_expo_settings() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path();
        for path in [
            ".expo/cache",
            ".m2/repository",
            "Library/Caches/ms-playwright",
            ".cache/puppeteer",
            "Library/Caches/node-gyp",
            "Library/Caches/typescript",
        ] {
            cache(home, path);
        }
        std::fs::write(home.join(".expo/state.json"), "credentials").unwrap();
        let inventory = inspect_ids(
            &Host::default(),
            &context(home),
            &[
                "expo.global-cache",
                "maven.repository",
                "playwright.browsers",
                "puppeteer.browsers",
                "node-gyp.headers",
                "typescript.cache",
            ],
        );
        assert_eq!(inventory.items.len(), 6);
        for item in &inventory.items {
            assert!(item.manually_selectable(), "{}", item.id);
            assert_eq!(item.properties.automation, Automation::Confirmed);
            assert!(!matches!(
                item.policy_status(CleanupLevel::Safe, false, true),
                crate::targets::PolicyStatus::Eligible
            ));
            assert!(matches!(
                item.operation,
                PlannedOperation::DirectDelete {
                    children_only: true,
                    ..
                }
            ));
            assert!(matches!(item.usage, UsageEstimate::Filesystem { .. }));
        }
        assert_eq!(inventory.scans.len(), 6);
        assert!(
            inventory
                .items
                .iter()
                .all(|item| item.path.as_deref() != Some(home.join(".expo").as_path()))
        );
    }
    #[test]
    fn maven_local_installs_and_cloud_or_symlink_overrides_are_preserved() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path();
        let maven = cache(home, ".m2/repository");
        std::fs::write(maven.join("maven-metadata-local.xml"), "local").unwrap();
        let cloud = cache(home, "Library/CloudStorage/provider/browsers");
        let mut context = context(home);
        context
            .variables
            .insert("PUPPETEER_CACHE_DIR".into(), cloud.display().to_string());
        let inventory = inspect_ids(
            &Host::default(),
            &context,
            &["maven.repository", "puppeteer.browsers"],
        );
        assert_eq!(inventory.items.len(), 2);
        assert!(
            inventory
                .items
                .iter()
                .all(|item| !item.manually_selectable())
        );
        #[cfg(unix)]
        {
            let link = home.join("linked-cache");
            std::os::unix::fs::symlink(&maven, &link).unwrap();
            context
                .variables
                .insert("PUPPETEER_CACHE_DIR".into(), link.display().to_string());
            assert!(
                !inspect_ids(&Host::default(), &context, &["puppeteer.browsers"]).items[0]
                    .manually_selectable()
            );
        }
    }
    #[test]
    fn gradle_requires_successful_daemon_inspection() {
        let fixture = tempfile::tempdir().unwrap();
        for path in [".gradle/caches", ".gradle/wrapper/dists", ".gradle/daemon"] {
            cache(fixture.path(), path);
        }
        let ids = ["gradle.caches", "gradle.wrapper", "gradle.daemon-logs"];
        assert!(
            inspect_ids(&Host::default(), &context(fixture.path()), &ids)
                .items
                .iter()
                .all(|item| !item.manually_selectable())
        );
        let host = Host {
            outputs: BTreeMap::from([
                ("gradle".into(), "Gradle fixture".into()),
                (
                    "gradle --offline --status".into(),
                    "No Gradle daemons are running.".into(),
                ),
            ]),
            ..Default::default()
        };
        let inventory = inspect_ids(&host, &context(fixture.path()), &ids);
        assert_eq!(inventory.items.len(), 3);
        assert!(
            inventory
                .items
                .iter()
                .all(DetectedTarget::manually_selectable)
        );
        assert_eq!(inventory.items[0].class, TargetClass::Network);
    }
    #[test]
    fn go_pins_native_commands_and_separates_build_from_module_downloads() {
        let fixture = tempfile::tempdir().unwrap();
        let build = cache(fixture.path(), "build-cache");
        let modules = cache(fixture.path(), "module-cache");
        let host = Host {
            outputs: BTreeMap::from([(
                "go".into(),
                serde_json::json!({"GOCACHE": build, "GOMODCACHE": modules}).to_string(),
            )]),
            ..Default::default()
        };
        let inventory = inspect_ids(
            &host,
            &context(fixture.path()),
            &["go.build-cache", "go.module-cache"],
        );
        assert_eq!(inventory.items.len(), 2);
        for (item, flag, key) in [
            (&inventory.items[0], "-cache", "GOCACHE"),
            (&inventory.items[1], "-modcache", "GOMODCACHE"),
        ] {
            let PlannedOperation::Command { command } = &item.operation else {
                panic!("native clean");
            };
            assert_eq!(command.args, ["clean", flag]);
            assert_eq!(
                command.environment[key],
                item.path.as_ref().unwrap().display().to_string()
            );
            assert_eq!(command.environment["GOTOOLCHAIN"], "local");
        }
        assert!(matches!(
            inventory.items[0].policy_status(CleanupLevel::Safe, false, true),
            crate::targets::PolicyStatus::Eligible
        ));
        assert!(!matches!(
            inventory.items[1].policy_status(CleanupLevel::Safe, false, true),
            crate::targets::PolicyStatus::Eligible
        ));
        assert!(host.commands.lock().unwrap().iter().all(|command| {
            command
                .environment
                .get("GOTOOLCHAIN")
                .is_some_and(|v| v == "local")
        }));
    }
    #[test]
    fn android_requires_installed_package_evidence_and_avds_are_never_selected() {
        let fixture = tempfile::tempdir().unwrap();
        cache(
            fixture.path(),
            "Library/Android/sdk/system-images/android-35/google_apis/arm64-v8a",
        );
        cache(fixture.path(), "Library/Android/sdk/build-tools/35.0.0");
        cache(fixture.path(), ".android/avd/phone.avd");
        let host = Host { outputs: BTreeMap::from([("sdkmanager".into(), "system-images;android-35;google_apis;arm64-v8a | 1 | image\nbuild-tools;35.0.0 | 1 | tools".into())]), ..Default::default() };
        let ids = [
            "android.system-images",
            "android.build-tools",
            "android.avds",
        ];
        let inventory = inspect_ids(&host, &context(fixture.path()), &ids);
        assert_eq!(inventory.items.len(), 3);
        for item in &inventory.items[..2] {
            let PlannedOperation::Command { command } = &item.operation else {
                panic!("native uninstall");
            };
            assert_eq!(command.args[1], "--uninstall");
            assert!(command.args[0].starts_with("--sdk_root="));
        }
        let avd = &inventory.items[2];
        assert_eq!(avd.class, TargetClass::Irreplaceable);
        assert!(!avd.manually_selectable());
        for level in [
            CleanupLevel::Safe,
            CleanupLevel::Standard,
            CleanupLevel::Aggressive,
        ] {
            assert!(!matches!(
                avd.policy_status(level, false, true),
                crate::targets::PolicyStatus::Eligible
            ));
        }
        let missing = inspect_ids(&Host::default(), &context(fixture.path()), &ids);
        assert!(missing.items.iter().all(|item| !item.manually_selectable()));
    }
    #[test]
    fn metro_files_are_exact_targets_and_watchman_swift_use_capability_checks() {
        let fixture = tempfile::tempdir().unwrap();
        let root = cache(fixture.path(), "tmp/metro-generated")
            .parent()
            .unwrap()
            .to_owned();
        std::fs::write(root.join("haste-map-fixture"), b"map").unwrap();
        cache(fixture.path(), "tmp/unrelated");
        cache(
            fixture.path(),
            "Library/Caches/org.swift.swiftpm/repositories",
        );
        let mut context = context(fixture.path());
        context
            .variables
            .insert("TMPDIR".into(), root.display().to_string());
        let host = Host {
            outputs: BTreeMap::from([
                (
                    "watchman".into(),
                    "{\"roots\":[\"/fixture/project\"]}".into(),
                ),
                ("swift".into(), "purge-cache".into()),
            ]),
            ..Default::default()
        };
        let inventory = inspect_ids(
            &host,
            &context,
            &["metro.cache", "watchman.watches", "swiftpm.repositories"],
        );
        assert_eq!(inventory.items.len(), 4);
        assert_eq!(
            inventory
                .items
                .iter()
                .filter(|item| item.id == "metro.cache")
                .count(),
            2
        );
        assert!(
            inventory
                .items
                .iter()
                .all(DetectedTarget::manually_selectable)
        );
        assert!(
            inventory
                .items
                .iter()
                .all(|item| item.path.as_ref() != Some(&root))
        );
        assert!(inventory.items.iter().any(|item| matches!(
            item.operation,
            PlannedOperation::DirectDelete {
                children_only: false,
                ..
            }
        )));
        let invalid = Host {
            outputs: BTreeMap::from([
                ("watchman".into(), "{\"error\":\"unsupported\"}".into()),
                ("swift".into(), "old help".into()),
            ]),
            ..Default::default()
        };
        let inventory = inspect_ids(
            &invalid,
            &context,
            &["watchman.watches", "swiftpm.repositories"],
        );
        assert!(
            inventory
                .items
                .iter()
                .all(|item| !item.manually_selectable())
        );
    }
    #[test]
    fn state_bundles_and_unverified_wallpaper_identifier_are_inventory_only() {
        let fixture = tempfile::tempdir().unwrap();
        cache(
            fixture.path(),
            "Virtual Machines.localized/fixture.vmwarevm",
        );
        cache(
            fixture.path(),
            "Library/Application Support/Claude/vm_bundles/claudevm.bundle",
        );
        let extension = cache(
            fixture.path(),
            "Library/Containers/com.apple.wallpaper.agent/Data/Library/Application Support/a/extension-com.apple.wallpaper.extension.image",
        );
        cache(
            fixture.path(),
            "Library/Containers/com.apple.wallpaper.agent/Data/Library/Caches/unrelated",
        );
        let host = Host {
            outputs: BTreeMap::from([("sw_vers".into(), "26.0".into())]),
            ..Default::default()
        };
        let inventory = inspect_ids(
            &host,
            &context(fixture.path()),
            &["vmware.bundles", "claude.cowork", "wallpaper.cache"],
        );
        assert_eq!(inventory.items.len(), 3);
        let wallpaper = inventory
            .items
            .iter()
            .find(|item| item.id == "wallpaper.cache")
            .unwrap();
        assert_eq!(wallpaper.path.as_ref(), Some(&extension));
        assert_eq!(wallpaper.provider.version.as_deref(), Some("26.0"));
        for item in inventory.items {
            assert!(!item.manually_selectable());
            for level in [
                CleanupLevel::Safe,
                CleanupLevel::Standard,
                CleanupLevel::Aggressive,
            ] {
                assert!(!matches!(
                    item.policy_status(level, false, true),
                    crate::targets::PolicyStatus::Eligible
                ));
            }
        }
        assert!(
            inspect_ids(
                &host,
                &SystemContext {
                    macos: false,
                    ..context(fixture.path())
                },
                &["wallpaper.cache"]
            )
            .items
            .is_empty()
        );
    }
}
