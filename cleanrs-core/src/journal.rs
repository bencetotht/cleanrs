//! Append-only cleanup history with restrictive permissions and bounded rotation.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::execution::{ActionResult, ActionStatus};

/// Default maximum journal size before the current file becomes `.1`.
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// One completed or aborted cleanup action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalAction {
    /// Evidence reviewed for project activity, retention, and ownership.
    #[serde(default)]
    pub evidence: Vec<String>,
    pub target_id: String,
    pub provider_id: String,
    pub path: Option<PathBuf>,
    pub context: Option<String>,
    pub argv: Option<Vec<String>>,
    pub status: String,
    /// Missing in pre-M0 journals; absence means selection by level.
    #[serde(default = "level_selection_source")]
    pub selection_source: String,
    #[serde(default)]
    pub overridden_skip_reason: Option<String>,
    pub planned_allocated: Option<u64>,
    pub estimated_floor: Option<u64>,
    pub estimated_ceiling: Option<u64>,
    pub provider_estimated: Option<u64>,
    pub removed_allocation: Option<u64>,
    pub provider_reported: Option<u64>,
    pub bytes_trashed: Option<u64>,
    pub exit_code: Option<i32>,
    pub stdout_summary: Option<String>,
    pub stderr_summary: Option<String>,
    pub error: Option<String>,
}

fn level_selection_source() -> String {
    "level".into()
}

impl JournalAction {
    /// Attach an execution result to action facts captured from the plan.
    pub fn finish(mut self, result: &ActionResult) -> Self {
        self.status = status_name(result.status).into();
        self.removed_allocation = result.removed_allocation;
        self.provider_reported = result.provider_reported;
        self.exit_code = result.exit_code;
        self.stdout_summary = result.stdout_summary.clone();
        self.stderr_summary = result.stderr_summary.clone();
        self.error = result.error.clone();
        self
    }
}

/// One invocation snapshot. A pending snapshot is appended before mutation and
/// newer snapshots retain action-level progress and partial failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalRun {
    pub schema: u32,
    pub run_id: String,
    pub timestamp_unix_seconds: u64,
    pub cleanrs_version: String,
    pub scope: PathBuf,
    pub level: String,
    pub explicitly_named_ids: Vec<String>,
    pub outcome: String,
    pub actions: Vec<JournalAction>,
}

/// Append-only JSONL journal.
#[derive(Debug, Clone)]
pub struct Journal {
    path: PathBuf,
    max_bytes: u64,
}

impl Journal {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one complete run, rotating one previous file when needed.
    pub fn append(&self, run: &JournalRun) -> io::Result<()> {
        let serialized = serde_json::to_vec(run).map_err(io::Error::other)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
            set_private_directory(parent)?;
        }
        let current_len = fs::metadata(&self.path).map_or(0, |metadata| metadata.len());
        if current_len.saturating_add(serialized.len() as u64 + 1) > self.max_bytes
            && self.path.exists()
        {
            let rotated = self.path.with_extension("jsonl.1");
            if rotated.exists() {
                fs::remove_file(&rotated)?;
            }
            fs::rename(&self.path, rotated)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&self.path)?;
        file.write_all(&serialized)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        set_private_file(&self.path)?;
        Ok(())
    }

    /// Read valid records. A malformed/truncated line is surfaced rather than
    /// silently discarded.
    pub fn read(&self) -> io::Result<Vec<JournalRun>> {
        let rotated = self.path.with_extension("jsonl.1");
        if !self.path.exists() && !rotated.exists() {
            return Ok(Vec::new());
        }
        let mut records = if rotated.exists() {
            read_file(&rotated)?
        } else {
            Vec::new()
        };
        if self.path.exists() {
            records.extend(read_file(&self.path)?);
        }
        let mut runs: Vec<JournalRun> = Vec::new();
        for record in records {
            if let Some(existing) = runs.iter().position(|run| run.run_id == record.run_id) {
                runs[existing] = record;
            } else {
                runs.push(record);
            }
        }
        Ok(runs)
    }
}

/// Resolve the journal without ever consulting the working directory.
#[must_use]
pub fn default_journal_path(home: Option<&Path>, state_home: Option<&Path>) -> Option<PathBuf> {
    state_home
        .map(|path| path.join("cleanrs/history.jsonl"))
        .or_else(|| home.map(|path| path.join(".local/state/cleanrs/history.jsonl")))
}

fn read_file(path: &Path) -> io::Result<Vec<JournalRun>> {
    let file = File::open(path)?;
    BufReader::new(file)
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let line = line?;
            serde_json::from_str(&line).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid journal record at line {}: {error}", index + 1),
                )
            })
        })
        .collect()
}

fn status_name(status: ActionStatus) -> &'static str {
    match status {
        ActionStatus::Success => "success",
        ActionStatus::Failed => "failure",
        ActionStatus::Skipped => "skipped",
        ActionStatus::ValidationAbort => "validation_abort",
    }
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn journal_round_trips_and_rotates() {
        let fixture = tempdir().unwrap();
        let path = fixture.path().join("history.jsonl");
        let journal = Journal::new(path.clone());
        let run = JournalRun {
            schema: 1,
            run_id: "fixture".into(),
            timestamp_unix_seconds: 1,
            cleanrs_version: "test".into(),
            scope: fixture.path().into(),
            level: "safe".into(),
            explicitly_named_ids: Vec::new(),
            outcome: "success".into(),
            actions: Vec::new(),
        };
        journal.append(&run).unwrap();
        assert_eq!(journal.read().unwrap()[0].run_id, "fixture");
        let mut second = run.clone();
        second.run_id = "second".into();
        Journal::new(path)
            .with_max_bytes(1)
            .append(&second)
            .unwrap();
        assert!(fixture.path().join("history.jsonl.1").exists());
        assert_eq!(journal.read().unwrap().len(), 2);
    }
    #[test]
    fn old_actions_without_selection_fields_still_parse() {
        let action: JournalAction = serde_json::from_value(serde_json::json!({
            "target_id": "fixture", "provider_id": "workspace", "status": "success"
        }))
        .unwrap();
        assert_eq!(action.selection_source, "level");
        assert_eq!(action.overridden_skip_reason, None);
        let fixture = tempdir().unwrap();
        let path = fixture.path().join("history.jsonl");
        fs::write(&path, serde_json::json!({
            "schema": 1, "run_id": "old", "timestamp_unix_seconds": 1,
            "cleanrs_version": "test", "scope": fixture.path(), "level": "safe",
            "explicitly_named_ids": [], "outcome": "success",
            "actions": [{"target_id": "fixture", "provider_id": "workspace", "status": "success"}]
        }).to_string() + "\n").unwrap();
        assert_eq!(
            Journal::new(path).read().unwrap()[0].actions[0].selection_source,
            "level"
        );
    }
}
