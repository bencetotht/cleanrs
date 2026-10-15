use super::*;
use crate::targets::{CandidateKey, PlanSelection, SelectionMode, plan_cleanup_with_selection};
use std::sync::Mutex;

struct Host {
    root: PathBuf,
    replies: BTreeMap<String, CommandOutput>,
    variables: BTreeMap<String, String>,
    commands: Mutex<Vec<CommandSpec>>,
}
impl Host {
    fn new(root: &Path) -> Self {
        Self {
            root: root.canonicalize().unwrap(),
            replies: BTreeMap::new(),
            variables: BTreeMap::new(),
            commands: Mutex::new(vec![]),
        }
    }
    fn reply(&mut self, key: &str, text: impl Into<String>) {
        self.replies.insert(
            key.into(),
            CommandOutput {
                exit_code: Some(0),
                stdout: text.into(),
                stderr: String::new(),
            },
        );
    }
    fn tree(&self, suffix: &str) -> PathBuf {
        let path = self.root.join(suffix);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("payload"), vec![42; 16384]).unwrap();
        path
    }
    fn inspect(&self, ids: &[&str]) -> ProviderInventory {
        inspect_builtin_providers(self, &ids.iter().map(|s| s.to_string()).collect())
    }
}
impl ProviderHost for Host {
    fn home(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }
    fn variable(&self, name: &str) -> Option<String> {
        self.variables.get(name).cloned()
    }
    fn resolve_executable(&self, name: &str) -> Result<Option<PathBuf>, ProviderError> {
        Ok(self
            .replies
            .keys()
            .any(|k| k.starts_with(&format!("{name} ")))
            .then(|| self.root.join("bin").join(name)))
    }
    fn run_inspection(&self, command: &CommandSpec) -> Result<CommandOutput, ProviderError> {
        self.commands.lock().unwrap().push(command.clone());
        let key = format!(
            "{} {}",
            command.executable.file_name().unwrap().to_string_lossy(),
            command.args.join(" ")
        );
        self.replies
            .get(&key)
            .cloned()
            .ok_or_else(|| ProviderError::new(format!("unsupported fixture command: {key}")))
    }
}
fn args(item: &DetectedTarget) -> Vec<String> {
    match &item.operation {
        PlannedOperation::Command { command } => command.args.clone(),
        _ => panic!("expected command: {item:?}"),
    }
}

#[test]
fn cache_inventory_distinguishes_partial_prune_and_reuses_union_facts() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    let cache = host.tree("uv-cache");
    host.reply("uv --version", "uv 0.9.0");
    host.reply("uv cache dir --no-config", cache.display().to_string());
    host.reply("uv cache size --no-config", "16384");
    let inventory = host.inspect(&["uv.cache-prune", "uv.cache-clean"]);
    assert_eq!(inventory.items.len(), 2);
    assert_eq!(
        inventory.scans.len(),
        1,
        "alternative scopes must share filesystem facts"
    );
    assert!(matches!(
        inventory.items[0].usage,
        UsageEstimate::Unknown { .. }
    ));
    assert!(matches!(
        inventory.items[1].usage,
        UsageEstimate::Filesystem { .. }
    ));
    for item in &inventory.items {
        assert_eq!(item.storage.len(), 1);
        assert_eq!(item.storage[0].apparent, 16384);
        assert_eq!(item.storage[0].path, cache);
        assert!(revalidate_storage(item, Some(&host.root)).is_ok());
        let PlannedOperation::Command { command } = &item.operation else {
            panic!()
        };
        assert_eq!(
            command.environment["UV_CACHE_DIR"],
            cache.display().to_string()
        );
    }
    std::fs::rename(&cache, host.root.join("old-cache")).unwrap();
    std::fs::create_dir(&cache).unwrap();
    assert!(revalidate_storage(&inventory.items[1], Some(&host.root)).is_err());
}

#[test]
fn npm_npx_capability_and_content_cache_have_separate_scopes() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    let root = host.root.join("npm");
    host.tree("npm/_cacache");
    host.tree("npm/_npx");
    host.reply("npm --version", "11.2.0");
    host.reply("npm config get cache", root.display().to_string());
    host.reply("npm cache --help", "npm cache npx rm [<key>...]");
    let inventory = host.inspect(&["npm.cache-verify", "npm.npx-cache"]);
    assert!(matches!(
        inventory.items[0].usage,
        UsageEstimate::Unknown { .. }
    ));
    assert_eq!(inventory.items[0].storage[0].path, root.join("_cacache"));
    let npx = &inventory.items[1];
    assert_eq!(npx.path, Some(root.join("_npx")));
    assert_eq!(&args(npx)[..4], &["cache", "npx", "rm", "--force"]);
    assert!(args(npx).contains(&format!("--cache={}", root.display())));
    host.reply("npm cache --help", "npm cache verify");
    let old = host.inspect(&["npm.npx-cache"]);
    assert!(matches!(
        old.items[0].operation,
        PlannedOperation::Informational
    ));
    assert_eq!(old.items[0].storage.len(), 1);
}

#[test]
fn yarn_classic_measures_the_parent_it_actually_cleans_and_modern_uses_mirror() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    let cache = host.tree("yarn/v6");
    host.tree("yarn/v4");
    host.reply("yarn --version", "1.22.22");
    host.reply("yarn cache dir", cache.display().to_string());
    let classic = host.inspect(&["yarn.cache-clean"]);
    assert_eq!(classic.items[0].path, Some(host.root.join("yarn")));
    assert_eq!(classic.items[0].storage[0].apparent, 32768);
    let PlannedOperation::Command { command } = &classic.items[0].operation else {
        panic!()
    };
    assert_eq!(
        command.environment["YARN_CACHE_FOLDER"],
        host.root.join("yarn").display().to_string()
    );
    assert_eq!(command.environment["COREPACK_ENABLE_NETWORK"], "0");
    let global = host.root.join("berry");
    host.tree("berry/cache");
    host.reply("yarn --version", "4.9.0");
    host.reply(
        "yarn config get globalFolder --json",
        serde_json::to_string(&global).unwrap(),
    );
    let modern = host.inspect(&["yarn.cache-clean"]);
    assert_eq!(modern.items[0].path, Some(global.join("cache")));
    assert_eq!(args(&modern.items[0]), ["cache", "clean", "--mirror"]);
    host.replies.get_mut("yarn --version").unwrap().exit_code = Some(1);
    let unavailable = host.inspect(&["yarn.cache-clean"]);
    assert!(!unavailable.items[0].manually_selectable());
}

#[test]
fn python_reports_are_not_parsed_as_bare_integers_and_scopes_are_pinned() {
    assert_eq!(
        reported_bytes(
            "pip.cache-purge",
            "Package index page cache size (http-v2): 1.2 MB\nLocally built wheels size: 800 kB\nLocally built wheels: 12"
        ),
        Some(2_000_000)
    );
    assert_eq!(
        reported_bytes("pip.cache-purge", "unsupported output"),
        None
    );
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    let cache = host.tree("pip");
    host.reply("pip3 --version", "pip 25.1");
    host.reply("pip3 cache dir --isolated", cache.display().to_string());
    host.reply(
        "pip3 cache info --isolated",
        "Package index page cache size: 1 MB\nLocally built wheels size: 1 MB",
    );
    let pip = host.inspect(&["pip.cache-purge"]);
    assert_eq!(pip.items[0].storage[0].path, cache);
    assert!(matches!(
        pip.items[0].usage,
        UsageEstimate::ProviderReported {
            bytes: Some(2_000_000),
            ..
        }
    ));
    assert!(args(&pip.items[0]).contains(&"--cache-dir".into()));
    let poetry = host.tree("poetry/cache/repositories/PyPI");
    host.tree("poetry/artifacts");
    host.reply("poetry --version", "Poetry 2.2.0");
    host.reply(
        "poetry config cache-dir",
        host.root.join("poetry").display().to_string(),
    );
    let inventory = host.inspect(&["poetry.cache-clean"]);
    assert_eq!(inventory.items[0].path, Some(poetry));
    assert_eq!(
        args(&inventory.items[0]),
        ["cache", "clear", "PyPI", "--all", "--no-interaction"]
    );
}

#[test]
fn conda_preserves_extracted_packages_and_protects_base_and_active_environments() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    let cache = host.tree("conda/pkgs");
    let base = host.tree("conda/base");
    let active = host.tree("conda/envs/active");
    let old = host.tree("conda/envs/old");
    host.reply("conda --version", "conda 25.5.0");
    host.reply(
        "conda info --json",
        serde_json::json!({"pkgs_dirs":[cache], "root_prefix":base, "active_prefix":active})
            .to_string(),
    );
    host.reply(
        "conda env list --json",
        serde_json::json!({"envs":[base,active,old]}).to_string(),
    );
    host.reply(
        "conda clean --tarballs --index-cache --logfiles --dry-run --json",
        r#"{"success":true,"tarballs":{"total_size":4096},"index_cache":[]}"#,
    );
    let inventory = host.inspect(&["conda.cache-clean", "conda.environments"]);
    let cache = inventory
        .items
        .iter()
        .find(|i| i.id == "conda.cache-clean")
        .unwrap();
    assert!(
        !args(cache)
            .iter()
            .any(|a| a == "--all" || a == "--packages")
    );
    assert!(matches!(cache.usage, UsageEstimate::Unknown { .. }));
    let environments: Vec<_> = inventory
        .items
        .iter()
        .filter(|i| i.id == "conda.environments")
        .collect();
    assert_eq!(environments.len(), 3);
    assert!(
        environments
            .iter()
            .filter(|i| i.path != Some(old.clone()))
            .all(|i| !i.manually_selectable())
    );
    let old = environments
        .iter()
        .find(|i| i.path == Some(old.clone()))
        .unwrap();
    assert!(old.manually_selectable());
    assert_eq!(&args(old)[..3], &["env", "remove", "--prefix"]);
    let plan = crate::targets::plan_cleanup(
        vec![(*old).clone()],
        CleanupLevel::Aggressive,
        &["conda.environments".into()],
        true,
    );
    assert!(
        matches!(plan[0].selection, PlanSelection::Skipped(_)),
        "an ID is not individual naming"
    );
    let keys = HashSet::from([CandidateKey::from_target(old)]);
    let selected = plan_cleanup_with_selection(
        vec![(*old).clone()],
        CleanupLevel::Safe,
        &[],
        false,
        &keys,
        SelectionMode::ManualOnly,
    )
    .unwrap();
    assert!(matches!(
        selected[0].selection,
        PlanSelection::Selected { .. }
    ));
}

#[test]
fn rustup_inventory_requires_individual_selection_and_retains_dependency_warning() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    host.tree(".rustup/toolchains/stable-x86_64-test");
    host.tree(".rustup/toolchains/old-x86_64-test");
    host.reply("rustup --version", "rustup 1.28.0");
    host.reply(
        "rustup toolchain list",
        "stable-x86_64-test (active, default)\nold-x86_64-test\n",
    );
    host.reply("rustup override list", "no overrides");
    let inventory = host.inspect(&["rustup.toolchains"]);
    assert_eq!(inventory.items.len(), 2);
    let old = inventory
        .items
        .iter()
        .find(|i| i.path.as_ref().unwrap().ends_with("old-x86_64-test"))
        .unwrap();
    assert_eq!(args(old), ["toolchain", "uninstall", "old-x86_64-test"]);
    assert!(
        old.evidence
            .iter()
            .any(|s| s.contains("dependencies outside"))
    );
    assert!(
        inventory
            .items
            .iter()
            .find(|i| i.path.as_ref().unwrap().ends_with("stable-x86_64-test"))
            .is_some_and(|i| !i.manually_selectable())
    );
}

#[test]
fn unsafe_or_missing_locations_never_authorize_a_guessed_cache_clean() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    host.reply("bun --version", "1.2.0");
    for location in ["relative/cache", "/", "no cache\n/cache"] {
        host.reply("bun pm cache", location);
        let inventory = host.inspect(&["bun.cache-clean"]);
        assert!(!inventory.items[0].manually_selectable(), "{location}");
    }
    let cloud = host.tree("Library/CloudStorage/example/cache");
    host.reply("bun pm cache", cloud.display().to_string());
    assert!(!host.inspect(&["bun.cache-clean"]).items[0].manually_selectable());
    #[cfg(unix)]
    {
        let actual = host.tree("actual-cache");
        let alias = host.root.join("alias");
        std::os::unix::fs::symlink(actual, &alias).unwrap();
        host.reply("bun pm cache", alias.display().to_string());
        assert!(!host.inspect(&["bun.cache-clean"]).items[0].manually_selectable());
    }
    host.reply(
        "bun pm cache",
        host.root.join("absent").display().to_string(),
    );
    let absent = host.inspect(&["bun.cache-clean"]);
    assert!(!absent.items[0].manually_selectable());
    assert!(
        absent.items[0]
            .evidence
            .iter()
            .any(|s| s.contains("storage absent"))
    );
    host.tree(".bun/install/cache");
    host.replies.insert(
        "bun pm cache".into(),
        CommandOutput {
            exit_code: Some(1),
            stdout: String::new(),
            stderr: "unsupported Bun cache option".into(),
        },
    );
    let failed = host.inspect(&["bun.cache-clean"]);
    assert!(!failed.items[0].manually_selectable());
    assert_eq!(failed.items[0].storage.len(), 1);
    assert!(
        failed.items[0]
            .evidence
            .iter()
            .any(|s| s.contains("unsupported Bun"))
    );
}

#[test]
fn xcode_inventory_measures_derived_data_and_reviews_unavailable_devices_individually() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    let uuid = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
    let derived = host.tree("Library/Developer/Xcode/DerivedData");
    host.tree(&format!("Library/Developer/CoreSimulator/Devices/{uuid}"));
    host.reply(
        "xcode-select -p",
        "/Applications/Xcode.app/Contents/Developer",
    );
    // Resolve xcrun without invoking it for detection.
    host.reply("xcrun simctl list devices unavailable --json", serde_json::json!({"devices":{"runtime":[{"udid":uuid,"name":"old phone","isAvailable":false}]}}).to_string());
    let inventory = host.inspect(&["xcode.derived-data", "xcode.simulators-unavailable"]);
    let data = inventory
        .items
        .iter()
        .find(|i| i.id == "xcode.derived-data")
        .unwrap();
    assert_eq!(data.path, Some(derived));
    assert!(data.explicit_only());
    let simulator = inventory
        .items
        .iter()
        .find(|i| i.id == "xcode.simulators-unavailable")
        .unwrap();
    assert_eq!(args(simulator), ["simctl", "delete", uuid]);
    assert!(simulator.explicit_only());
    assert!(matches!(simulator.usage, UsageEstimate::Filesystem { .. }));
    host.reply(
        "xcrun simctl list devices unavailable --json",
        r#"{"devices":{"runtime":[{"udid":"../escape","isAvailable":false}]}}"#,
    );
    assert!(!host.inspect(&["xcode.simulators-unavailable"]).items[0].manually_selectable());
}

#[test]
fn cocoapods_resolves_user_cache_and_never_initializes_it_during_inventory() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    let pods = host.tree("custom-pods/Pods");
    std::fs::create_dir_all(host.root.join(".cocoapods")).unwrap();
    std::fs::write(
        host.root.join(".cocoapods/config.yaml"),
        "cache_root: '~/custom-pods'\n",
    )
    .unwrap();
    host.reply("pod --version", "1.16.2");
    let inventory = host.inspect(&["cocoapods.cache-clean"]);
    assert_eq!(inventory.items[0].path, Some(pods));
    assert_eq!(args(&inventory.items[0]), ["cache", "clean", "--all"]);
    assert_eq!(
        host.commands.lock().unwrap().len(),
        1,
        "only version inspection may invoke CocoaPods"
    );
    std::fs::write(
        host.root.join(".cocoapods/config.yaml"),
        "cache_root: &arbitrary /elsewhere\n",
    )
    .unwrap();
    assert!(!host.inspect(&["cocoapods.cache-clean"]).items[0].manually_selectable());
}

#[test]
fn conda_invalid_or_incomplete_json_never_becomes_zero_cleanup_yield() {
    for text in [
        "0",
        "{}",
        r#"{"success":false}"#,
        r#"{"success":true,"tarballs":{"total_size":"unknown"}}"#,
    ] {
        assert!(inspection_evidence("conda.cache-clean", text).is_err());
    }
    let evidence = inspection_evidence(
        "conda.cache-clean",
        r#"{"success":true,"tarballs":{"total_size":128,"warnings":["unreadable archive"]}}"#,
    )
    .unwrap();
    assert!(evidence.iter().any(|s| s.contains("128 bytes")));
    assert!(evidence.iter().any(|s| s.contains("unreadable archive")));
}

#[test]
fn cargo_shared_inventory_never_counts_installed_binaries_as_disposable_cache() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    host.tree(".cargo/registry");
    host.tree(".cargo/git");
    host.tree(".cargo/bin");
    host.reply("cargo --version", "cargo 1.90.0");
    let inventory = host.inspect(&["cargo.shared-cache"]);
    assert_eq!(inventory.items[0].storage.len(), 2);
    assert!(!inventory.items[0].manually_selectable());
    assert!(
        inventory.items[0]
            .storage
            .iter()
            .all(|s| !s.path.ends_with("bin"))
    );
}

#[test]
fn pnpm_pins_the_store_root_without_appending_the_schema_version_twice() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    let store = host.tree("pnpm/v10");
    host.reply("pnpm --version", "10.8.0");
    host.reply("pnpm store path", store.display().to_string());
    host.reply(
        &format!(
            "pnpm store path --store-dir {}",
            host.root.join("pnpm").display()
        ),
        store.display().to_string(),
    );
    let inventory = host.inspect(&["pnpm.store-prune"]);
    assert!(inventory.items[0].manually_selectable());
    assert_eq!(
        args(&inventory.items[0]),
        [
            "store",
            "prune",
            "--store-dir",
            host.root.join("pnpm").to_str().unwrap()
        ]
    );
    assert!(matches!(
        inventory.items[0].usage,
        UsageEstimate::Unknown { .. }
    ));
    assert_eq!(inventory.items[0].storage[0].path, store);
}

#[test]
fn homebrew_and_nix_measure_removable_inventory_without_promising_whole_store_yield() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = Host::new(temp.path());
    let cache = host.tree("brew/cache");
    let cellar = host.tree("brew/cellar/example");
    host.reply("brew --version", "Homebrew 6.0.18");
    host.reply(
        "brew --prefix",
        host.root.join("brew").display().to_string(),
    );
    host.reply("brew --cache", cache.display().to_string());
    host.reply(
        "brew cleanup --dry-run",
        "This operation would free approximately 2MB of disk space.",
    );
    host.reply(
        "brew autoremove --dry-run",
        "Would autoremove 1 unneeded formula:\nexample\n",
    );
    host.reply("brew --cellar example", cellar.display().to_string());
    let inventory = host.inspect(&["homebrew.cleanup", "homebrew.autoremove"]);
    assert_eq!(inventory.items[0].storage[0].path, cache);
    assert!(matches!(
        inventory.items[0].usage,
        UsageEstimate::ProviderReported {
            bytes: Some(2_000_000),
            ..
        }
    ));
    assert_eq!(inventory.items[1].storage[0].path, cellar);
    assert!(matches!(
        inventory.items[1].usage,
        UsageEstimate::Unknown { .. }
    ));
    let dead = host.tree("nix-store/dead");
    host.reply("nix-collect-garbage --version", "fixture");
    host.reply("nix-store --gc --print-dead", dead.display().to_string());
    let nix = host.inspect(&["nix.gc"]);
    assert_eq!(nix.items[0].storage[0].path, dead);
    assert!(matches!(nix.items[0].usage, UsageEstimate::Unknown { .. }));
}
