//! Persistent trust for explicitly named command configuration.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TrustRecord {
    pub canonical_path: PathBuf,
    pub sha256: String,
    pub trusted_unix_seconds: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    #[serde(default)]
    records: Vec<TrustRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct TrustStore {
    path: PathBuf,
}

impl TrustStore {
    pub(crate) fn for_home(home: &Path) -> Self {
        Self {
            path: home.join(".config/cleanrs/trusted-configs.json"),
        }
    }

    pub(crate) fn trust(&self, config: &Path) -> Result<TrustRecord> {
        let record = record_for(config)?;
        let mut file = self.load()?;
        file.records
            .retain(|existing| existing.canonical_path != record.canonical_path);
        file.records.push(record.clone());
        file.records
            .sort_by(|left, right| left.canonical_path.cmp(&right.canonical_path));
        self.save(&file)?;
        Ok(record)
    }

    pub(crate) fn revoke(&self, config: &Path) -> Result<bool> {
        let canonical = match config.canonicalize() {
            Ok(path) => path,
            Err(_) if config.is_absolute() => config.to_path_buf(),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to resolve config {}; use its canonical absolute path to revoke a deleted file",
                        config.display()
                    )
                });
            }
        };
        let mut file = self.load()?;
        let before = file.records.len();
        file.records
            .retain(|record| record.canonical_path != canonical);
        if before != file.records.len() {
            self.save(&file)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn records(&self) -> Result<Vec<TrustRecord>> {
        Ok(self.load()?.records)
    }

    /// Return the currently trusted hash. Loading code checks this exact hash
    /// against the bytes it parses, closing the edit-between-check-and-load gap.
    pub(crate) fn trusted_hash(&self, config: &Path) -> Result<Option<String>> {
        let canonical = config
            .canonicalize()
            .with_context(|| format!("failed to resolve config {}", config.display()))?;
        let actual = hash_file(&canonical)?;
        Ok(self
            .load()?
            .records
            .into_iter()
            .find(|record| record.canonical_path == canonical && record.sha256 == actual)
            .map(|record| record.sha256))
    }

    fn load(&self) -> Result<TrustFile> {
        if !self.path.exists() {
            return Ok(TrustFile::default());
        }
        let bytes = fs::read(&self.path)
            .with_context(|| format!("failed to read trust store {}", self.path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse trust store {}", self.path.display()))
    }

    fn save(&self, trust: &TrustFile) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("trust store path has no parent")?;
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let temporary = parent.join(format!(
            ".trusted-configs.{}.{nonce}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut output = options.open(&temporary).with_context(|| {
            format!(
                "failed to create temporary trust store {}",
                temporary.display()
            )
        })?;
        let bytes = serde_json::to_vec_pretty(trust)?;
        output.write_all(&bytes)?;
        output.write_all(b"\n")?;
        output.sync_all()?;
        drop(output);
        if let Err(error) = fs::rename(&temporary, &self.path) {
            let _ = fs::remove_file(&temporary);
            return Err(error).context("failed to atomically replace trust store");
        }
        #[cfg(unix)]
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hex
}

fn hash_file(path: &Path) -> Result<String> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read config {}", path.display()))?;
    Ok(hash_bytes(&bytes))
}

fn record_for(path: &Path) -> Result<TrustRecord> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to resolve config {}", path.display()))?;
    if !canonical.is_file() {
        bail!(
            "trusted config must be a regular file: {}",
            canonical.display()
        );
    }
    Ok(TrustRecord {
        sha256: hash_file(&canonical)?,
        canonical_path: canonical,
        trusted_unix_seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
    })
}
pub(crate) fn resolve_explicit_config_trust(
    home: Option<&Path>,
    config: Option<&Path>,
    trust_now: bool,
) -> Result<Option<String>> {
    let Some(config) = config else {
        return Ok(None);
    };
    let Some(home) = home else {
        if trust_now {
            bail!("HOME is required to persist config trust");
        }
        return Ok(None);
    };
    let store = TrustStore::for_home(home);
    if trust_now {
        store.trust(config)?;
    }
    store.trusted_hash(config)
}
