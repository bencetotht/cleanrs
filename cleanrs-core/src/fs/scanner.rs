use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;

use super::{
    Arena, EntryMetadata, FileSystem, NativeFileSystem, Node, NodeFlags, NodeIndex, NodeKind,
};

/// Scanner configuration. The default never crosses mounts or follows links.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub cross_mounts: bool,
    pub workers: usize,
    pub excludes: Vec<String>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            cross_mounts: false,
            workers: default_worker_count(),
            excludes: Vec::new(),
        }
    }
}

/// A location where traversal could not obtain complete metadata.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageError {
    pub path: PathBuf,
    pub kind: String,
    pub message: String,
}

/// Completed read-only scan.
#[derive(Debug, Serialize)]
pub struct ScanResult {
    pub root: PathBuf,
    pub arena: Arena,
    pub coverage_errors: Vec<CoverageError>,
    pub backend: String,
    pub dataless_entries: usize,
    pub mount_boundaries: usize,
}

impl ScanResult {
    /// True only when every scheduled directory was readable.
    #[must_use]
    pub fn coverage_complete(&self) -> bool {
        self.coverage_errors.is_empty()
    }
}

/// Parallel, bounded directory scanner over a platform filesystem abstraction.
pub struct Scanner<F = NativeFileSystem> {
    filesystem: Arc<F>,
    options: ScanOptions,
}

impl Scanner<NativeFileSystem> {
    /// Construct a native scanner.
    #[must_use]
    pub fn new(options: ScanOptions) -> Self {
        Self {
            filesystem: Arc::new(NativeFileSystem),
            options,
        }
    }
}

impl<F: FileSystem> Scanner<F> {
    /// Construct a scanner with a custom backend, useful for capability tests.
    #[must_use]
    pub fn with_filesystem(filesystem: F, options: ScanOptions) -> Self {
        Self {
            filesystem: Arc::new(filesystem),
            options,
        }
    }

    /// Scan an explicit path. Content is never opened and symlinks are never followed.
    pub fn scan(&self, root: &Path) -> io::Result<ScanResult> {
        if std::fs::symlink_metadata(root)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scan root must not be a symbolic link",
            ));
        }
        let root = root.canonicalize()?;
        let root_metadata = self.filesystem.metadata(&root)?;
        if root_metadata.kind != NodeKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scan root must be a directory",
            ));
        }
        let root_device = root_metadata.device;
        let mut initial_arena = Arena::new(&root);
        initial_arena.nodes[0] = node_from_metadata(root_metadata);

        let arena = Arc::new(Mutex::new(initial_arena));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let dataless_entries = Arc::new(AtomicUsize::new(0));
        let mount_boundaries = Arc::new(AtomicUsize::new(0));
        let outstanding = Arc::new(AtomicUsize::new(1));
        let excludes = Arc::new(build_excludes(&self.options.excludes)?);
        let (sender, receiver) = crossbeam_channel::unbounded();
        sender
            .send(Job {
                path: root.clone(),
                parent: NodeIndex::from_usize(0),
            })
            .map_err(|_| io::Error::other("scanner queue closed"))?;

        std::thread::scope(|scope| {
            for _ in 0..self.options.workers.max(1) {
                let context = WorkerContext {
                    filesystem: Arc::clone(&self.filesystem),
                    arena: Arc::clone(&arena),
                    errors: Arc::clone(&errors),
                    dataless_entries: Arc::clone(&dataless_entries),
                    mount_boundaries: Arc::clone(&mount_boundaries),
                    outstanding: Arc::clone(&outstanding),
                    excludes: Arc::clone(&excludes),
                    sender: sender.clone(),
                    receiver: receiver.clone(),
                    root_device,
                    scan_root: root.clone(),
                    cross_mounts: self.options.cross_mounts,
                };
                scope.spawn(move || worker_loop(&context));
            }
        });

        // All worker-held Arcs are gone after the scoped threads join.
        let arena = Arc::try_unwrap(arena)
            .expect("scanner retained arena")
            .into_inner()
            .unwrap();
        let coverage_errors = Arc::try_unwrap(errors)
            .expect("scanner retained errors")
            .into_inner()
            .unwrap();
        Ok(ScanResult {
            root,
            arena,
            coverage_errors,
            backend: self.filesystem.backend_name().to_owned(),
            dataless_entries: dataless_entries.load(Ordering::Relaxed),
            mount_boundaries: mount_boundaries.load(Ordering::Relaxed),
        })
    }
}

#[derive(Debug)]
struct Job {
    path: PathBuf,
    parent: NodeIndex,
}

struct WorkerContext<F> {
    filesystem: Arc<F>,
    arena: Arc<Mutex<Arena>>,
    errors: Arc<Mutex<Vec<CoverageError>>>,
    dataless_entries: Arc<AtomicUsize>,
    mount_boundaries: Arc<AtomicUsize>,
    outstanding: Arc<AtomicUsize>,
    excludes: Arc<GlobSet>,
    sender: Sender<Job>,
    receiver: Receiver<Job>,
    root_device: u64,
    scan_root: PathBuf,
    cross_mounts: bool,
}

fn worker_loop<F: FileSystem>(context: &WorkerContext<F>) {
    loop {
        let job = match context.receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(job) => job,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if context.outstanding.load(Ordering::Acquire) == 0 {
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        process_job(context, job);
        context.outstanding.fetch_sub(1, Ordering::AcqRel);
    }
}

fn process_job<F: FileSystem>(context: &WorkerContext<F>, job: Job) {
    let entries = match context.filesystem.read_dir(&job.path) {
        Ok(entries) => entries,
        Err(error) => {
            context.errors.lock().unwrap().push(CoverageError {
                path: job.path,
                kind: io_error_kind(&error).into(),
                message: error.to_string(),
            });
            return;
        }
    };

    let mut prepared = Vec::with_capacity(entries.len());
    for entry in entries {
        let path = job.path.join(&entry.name);
        if context.excludes.is_match(&path) {
            continue;
        }
        let mut metadata = entry.metadata;
        let is_mount_boundary = metadata.device != context.root_device;
        if is_mount_boundary {
            metadata.flags.insert(NodeFlags::MOUNT_BOUNDARY);
            context.mount_boundaries.fetch_add(1, Ordering::Relaxed);
        }
        if metadata.flags.contains(NodeFlags::DATALESS) {
            context.dataless_entries.fetch_add(1, Ordering::Relaxed);
        }
        let is_directory = metadata.kind == NodeKind::Directory;
        let is_firmlink = metadata.flags.contains(NodeFlags::FIRMLINK);
        prepared.push((
            entry.name,
            path,
            metadata,
            is_directory,
            is_firmlink,
            is_mount_boundary,
        ));
    }

    let mut child_jobs = Vec::new();
    let mut arena = context.arena.lock().unwrap();
    for (name, path, metadata, is_directory, is_firmlink, is_mount_boundary) in prepared {
        let child_index = arena.push_child(
            job.parent,
            &name.to_string_lossy(),
            node_from_metadata(metadata),
        );
        // Topology and the flag are evaluated together. Visible root firmlinks
        // (for example /Users) are the canonical user-facing path and may be
        // traversed; their /System/Volumes/Data counterparts are refused so the
        // same tree cannot enter the arena twice.
        let topology_refused = refuses_topology(&context.scan_root, &path, is_firmlink);
        if is_directory && !topology_refused && (!is_mount_boundary || context.cross_mounts) {
            child_jobs.push(Job {
                path,
                parent: child_index,
            });
        }
    }
    drop(arena);

    for child in child_jobs {
        context.outstanding.fetch_add(1, Ordering::AcqRel);
        if context.sender.send(child).is_err() {
            context.outstanding.fetch_sub(1, Ordering::AcqRel);
            return;
        }
    }
}

fn refuses_topology(scan_root: &Path, candidate: &Path, is_firmlink: bool) -> bool {
    if scan_root == Path::new("/") {
        if candidate.starts_with("/System/Volumes/Data") || candidate == Path::new("/Volumes") {
            return true;
        }
        // On the sealed-system layout these are visible entrances into Data,
        // and are preferable to traversing the backing Data mount directly.
        return is_firmlink && !is_known_root_firmlink(candidate);
    }
    is_firmlink
}

fn is_known_root_firmlink(path: &Path) -> bool {
    matches!(
        path.to_str(),
        Some(
            "/AppleInternal"
                | "/Applications"
                | "/Library"
                | "/System/Library/Caches"
                | "/System/Library/Assets"
                | "/System/Library/PreinstalledAssets"
                | "/System/Library/AssetsV2"
                | "/System/Library/PreinstalledAssetsV2"
                | "/System/Library/CoreServices/CoreTypes.bundle/Contents/Library"
                | "/System/Library/Speech"
                | "/Users"
                | "/cores"
                | "/opt"
                | "/pkg"
                | "/private"
                | "/usr/local"
                | "/usr/libexec/cups"
                | "/usr/share/snmp"
        )
    )
}

fn node_from_metadata(metadata: EntryMetadata) -> Node {
    Node {
        kind: metadata.kind,
        apparent_size: metadata.apparent_size,
        allocated_size: metadata.allocated_size,
        private_size: metadata.private_size,
        device: metadata.device,
        inode: metadata.inode,
        link_count: metadata.link_count,
        clone_id: metadata.clone_id,
        clone_refcount: metadata.clone_refcount,
        modified_unix_seconds: metadata.modified_unix_seconds,
        flags: metadata.flags,
        ..Node::default()
    }
}

fn build_excludes(patterns: &[String]) -> io::Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    // Pseudo-filesystems and metadata stores are traps, not useful scan data.
    for pattern in ["**/.fseventsd/**", "**/.Spotlight-V100/**", "/dev/**"] {
        builder.add(Glob::new(pattern).expect("built-in glob is valid"));
    }
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid exclude {pattern:?}: {error}"),
            )
        })?);
    }
    builder.build().map_err(io::Error::other)
}

fn io_error_kind(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::NotFound => "disappeared_during_scan",
        _ => "metadata_error",
    }
}

fn default_worker_count() -> usize {
    // Conservative until Phase 0 produces hardware-specific benchmark data.
    std::thread::available_parallelism().map_or(2, |count| count.get().clamp(1, 8))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn scans_fixture_without_following_symlinks() {
        let fixture = tempdir().unwrap();
        fs::create_dir(fixture.path().join("dir")).unwrap();
        fs::write(fixture.path().join("dir/file"), b"payload").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("dir", fixture.path().join("link")).unwrap();

        let result = Scanner::new(ScanOptions::default())
            .scan(fixture.path())
            .unwrap();
        assert!(result.arena.nodes.len() >= 3);
        let symlinks = result
            .arena
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Symlink)
            .count();
        #[cfg(unix)]
        assert_eq!(symlinks, 1);
    }

    #[test]
    fn topology_keeps_visible_root_and_refuses_data_duplicate() {
        assert!(!refuses_topology(Path::new("/"), Path::new("/Users"), true));
        assert!(refuses_topology(
            Path::new("/"),
            Path::new("/System/Volumes/Data/Users"),
            false
        ));
        assert!(refuses_topology(
            Path::new("/"),
            Path::new("/Volumes"),
            true
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn allocated_bytes_agree_with_du_for_plain_files() {
        use std::process::Command;

        let fixture = tempdir().unwrap();
        fs::create_dir(fixture.path().join("nested")).unwrap();
        fs::write(fixture.path().join("small"), vec![1_u8; 17]).unwrap();
        fs::write(fixture.path().join("nested/large"), vec![2_u8; 65_537]).unwrap();

        let result = Scanner::new(ScanOptions::default())
            .scan(fixture.path())
            .unwrap();
        let ours: u64 = result
            .arena
            .nodes
            .iter()
            .filter(|node| node.kind.is_file_like())
            .map(|node| node.allocated_size)
            .sum();
        let output = Command::new("du")
            .args(["-sk", fixture.path().to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success());
        let du_kib: u64 = String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(ours, du_kib * 1024);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn real_mutual_clone_retains_the_allocated_ceiling() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        unsafe extern "C" {
            fn clonefile(
                source: *const libc::c_char,
                target: *const libc::c_char,
                flags: u32,
            ) -> libc::c_int;
        }

        let fixture = tempdir().unwrap();
        let original = fixture.path().join("original");
        let clone = fixture.path().join("clone");
        fs::write(&original, vec![7_u8; 1024 * 1024]).unwrap();
        let source = CString::new(original.as_os_str().as_bytes()).unwrap();
        let target = CString::new(clone.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { clonefile(source.as_ptr(), target.as_ptr(), 0) }, 0);

        let result = Scanner::new(ScanOptions::default())
            .scan(fixture.path())
            .unwrap();
        let estimate = crate::accounting::estimate_subtree(
            &result.arena,
            result.arena.root(),
            result.coverage_complete(),
        );
        assert_eq!(estimate.floor, Some(0));
        assert!(estimate.ceiling.unwrap_or_default() >= 1024 * 1024);
        assert!(
            result
                .arena
                .nodes
                .iter()
                .filter_map(|node| node.clone_id)
                .count()
                >= 2
        );
    }
}
