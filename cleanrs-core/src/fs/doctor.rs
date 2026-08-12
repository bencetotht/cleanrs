use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

use serde::Serialize;

/// One mounted volume relevant to container reconciliation.
#[derive(Debug, Clone, Serialize)]
pub struct VolumeReport {
    pub name: String,
    pub mount_point: PathBuf,
    pub capacity: u64,
    pub free: u64,
    pub used: u64,
    pub role: String,
}

/// Cheap environment and storage diagnostics used by `cleanrs doctor`.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub container: Option<String>,
    pub capacity: u64,
    pub free_shared: u64,
    pub used: u64,
    pub volumes: Vec<VolumeReport>,
    pub snapshot_count: Option<usize>,
    pub purgeable: Option<u64>,
    pub attributed_allocated: Option<u64>,
    pub unattributed: Option<u64>,
    pub scan_coverage_complete: Option<bool>,
    pub full_disk_access: String,
    pub notes: Vec<String>,
}

/// Inspect volume/container state without traversing user content.
pub fn doctor() -> io::Result<DoctorReport> {
    let root = volume_for(Path::new("/"), "System", "read-only system")?;
    let mut volumes = vec![root.clone()];
    for (path, name, role) in [
        ("/System/Volumes/Data", "Data", "writable data"),
        ("/System/Volumes/VM", "VM", "swap, OS-managed"),
        ("/nix", "Nix", "Nix store"),
    ] {
        let path = Path::new(path);
        if path.exists() {
            volumes.push(volume_for(path, name, role)?);
        }
    }

    let (container, container_capacity, container_free) = macos_container_info();
    let capacity = container_capacity.unwrap_or(root.capacity);
    let free_shared = container_free.unwrap_or(root.free);
    let snapshot_count = local_snapshot_count();
    let mut notes = vec![
        "Purgeable and snapshot storage overlap and are not added together.".into(),
        "Unexplained space remains explicit until a filesystem scan is supplied.".into(),
        "Full Disk Access: System Settings > Privacy & Security > Full Disk Access; add the terminal or cleanrs binary.".into(),
    ];
    if volumes.iter().any(|volume| volume.name == "VM") {
        notes.push("VM volume is reported diagnostically; cleanrs never deletes swap.".into());
    }
    if volumes.iter().any(|volume| volume.name == "Nix") {
        notes.push("Nix is a distinct volume and requires a provider-owned cleanup plan.".into());
    }

    Ok(DoctorReport {
        container,
        capacity,
        free_shared,
        used: capacity.saturating_sub(free_shared),
        volumes,
        snapshot_count,
        // diskutil's purgeable fields are version-dependent. Reporting unknown
        // is preferable to parsing a similarly named but different quantity.
        purgeable: None,
        attributed_allocated: None,
        unattributed: None,
        scan_coverage_complete: None,
        full_disk_access:
            "not probed (grant status cannot be inferred reliably without reading protected data)"
                .into(),
        notes,
    })
}

fn volume_for(path: &Path, name: &str, role: &str) -> io::Result<VolumeReport> {
    let stats = statvfs(path)?;
    let capacity = stats.blocks.saturating_mul(stats.fragment_size);
    let free = stats.blocks_available.saturating_mul(stats.fragment_size);
    let used = macos_volume_used(path).unwrap_or_else(|| capacity.saturating_sub(free));
    Ok(VolumeReport {
        name: name.into(),
        mount_point: path.into(),
        capacity,
        free,
        used,
        role: role.into(),
    })
}

struct FileSystemStats {
    blocks: u64,
    blocks_available: u64,
    fragment_size: u64,
}

#[allow(clippy::useless_conversion)]
fn statvfs(path: &Path) -> io::Result<FileSystemStats> {
    use std::ffi::CString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;

    #[cfg(unix)]
    {
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let mut raw = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(c_path.as_ptr(), raw.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let raw = unsafe { raw.assume_init() };
        Ok(FileSystemStats {
            blocks: u64::from(raw.f_blocks),
            blocks_available: u64::from(raw.f_bavail),
            fragment_size: u64::from(raw.f_frsize),
        })
    }

    #[cfg(not(unix))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "doctor requires a Unix filesystem",
    ))
}

#[cfg(target_os = "macos")]
fn macos_container_info() -> (Option<String>, Option<u64>, Option<u64>) {
    let output = Command::new("diskutil").args(["info", "/"]).output();
    let Ok(output) = output else {
        return (None, None, None);
    };
    if !output.status.success() {
        return (None, None, None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let container = field(&text, "APFS Container").map(ToOwned::to_owned);
    let capacity = byte_field(&text, "Container Total Space");
    let free = byte_field(&text, "Container Free Space");
    (container, capacity, free)
}

#[cfg(not(target_os = "macos"))]
fn macos_container_info() -> (Option<String>, Option<u64>, Option<u64>) {
    (None, None, None)
}

#[cfg(any(target_os = "macos", test))]
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (candidate, value) = line.split_once(':')?;
        (candidate.trim() == key).then(|| value.trim())
    })
}

#[cfg(any(target_os = "macos", test))]
fn byte_field(text: &str, key: &str) -> Option<u64> {
    let value = field(text, key)?;
    let start = value.find('(')? + 1;
    value[start..].split_whitespace().next()?.parse().ok()
}

#[cfg(target_os = "macos")]
fn macos_volume_used(path: &Path) -> Option<u64> {
    let output = Command::new("diskutil")
        .args(["info", path.to_str()?])
        .output()
        .ok()?;
    output.status.success().then(|| {
        byte_field(
            &String::from_utf8_lossy(&output.stdout),
            "Volume Used Space",
        )
    })?
}

#[cfg(not(target_os = "macos"))]
fn macos_volume_used(_path: &Path) -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn local_snapshot_count() -> Option<usize> {
    let output = Command::new("tmutil")
        .args(["listlocalsnapshots", "/"])
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.starts_with("com.apple.TimeMachine."))
            .count()
    })
}

#[cfg(not(target_os = "macos"))]
fn local_snapshot_count() -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_diskutil_byte_fields() {
        let fixture = "   Container Total Space:  494.4 GB (494384795648 Bytes)\n";
        assert_eq!(
            byte_field(fixture, "Container Total Space"),
            Some(494_384_795_648)
        );
    }
}
