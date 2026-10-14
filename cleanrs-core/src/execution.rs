//! Guarded execution primitives for immutable cleanup plans.
//!
//! Direct deletion is descriptor-relative on Unix: recursive traversal never
//! resolves a child through an absolute path and never follows a symbolic link.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use serde::Serialize;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::fs::NodeKind;
use crate::targets::{CandidateIdentity, CommandSpec};

/// Terminal state of one attempted cleanup action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Success,
    Failed,
    Skipped,
    ValidationAbort,
}

/// Evidence produced by one action. Allocation removed is deliberately not
/// labelled as host free-space recovery.
#[derive(Debug, Clone, Serialize)]
pub struct ActionResult {
    pub status: ActionStatus,
    pub removed_allocation: Option<u64>,
    pub provider_reported: Option<u64>,
    pub exit_code: Option<i32>,
    pub stdout_summary: Option<String>,
    pub stderr_summary: Option<String>,
    pub error: Option<String>,
}

impl ActionResult {
    #[must_use]
    pub fn validation_abort(reason: impl Into<String>) -> Self {
        Self {
            status: ActionStatus::ValidationAbort,
            removed_allocation: None,
            provider_reported: None,
            exit_code: None,
            stdout_summary: None,
            stderr_summary: None,
            error: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: ActionStatus::Failed,
            removed_allocation: None,
            provider_reported: None,
            exit_code: None,
            stdout_summary: None,
            stderr_summary: None,
            error: Some(reason.into()),
        }
    }
}

/// Stable identity for an executable captured after planning and checked again
/// immediately before spawning it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExecutableIdentity {
    pub device: u64,
    pub inode: u64,
    pub kind: NodeKind,
    pub resolved_device: u64,
    pub resolved_inode: u64,
    pub resolved_kind: NodeKind,
}

/// Capture executable identity without following a final symlink.
pub fn executable_identity(path: &Path) -> io::Result<ExecutableIdentity> {
    let entry = fs::symlink_metadata(path)?;
    let resolved = fs::metadata(path)?;
    #[cfg(unix)]
    {
        Ok(ExecutableIdentity {
            device: entry.dev(),
            inode: entry.ino(),
            kind: kind_from_metadata(&entry),
            resolved_device: resolved.dev(),
            resolved_inode: resolved.ino(),
            resolved_kind: kind_from_metadata(&resolved),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (entry, resolved);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "executable identity is currently supported only on Unix",
        ))
    }
}

/// Permanently remove a target after matching its planned identity.
///
/// `removed_allocation` comes from the reviewed scan. It is evidence about the
/// removed tree, not a claim about the volume's free-space delta.
pub fn execute_direct_delete(
    path: &Path,
    children_only: bool,
    planned: CandidateIdentity,
    removed_allocation: u64,
) -> ActionResult {
    match direct_delete(path, children_only, planned) {
        Ok(()) => ActionResult {
            status: ActionStatus::Success,
            removed_allocation: Some(removed_allocation),
            provider_reported: None,
            exit_code: None,
            stdout_summary: None,
            stderr_summary: None,
            error: None,
        },
        Err(DeleteError::Validation(reason)) => ActionResult::validation_abort(reason),
        Err(DeleteError::Io(error)) => ActionResult::failed(error.to_string()),
    }
}

/// Execute an argv-array command after executable identity revalidation.
/// Output is always captured to a bounded summary and can additionally be
/// streamed for an interactive terminal.
pub fn execute_command(
    command: &CommandSpec,
    planned_executable: ExecutableIdentity,
    stream: bool,
    max_summary_bytes: usize,
) -> ActionResult {
    execute_command_with_observer(
        command,
        planned_executable,
        max_summary_bytes,
        |stream_kind, line| {
            if stream {
                match stream_kind {
                    OutputStream::Stdout => {
                        let _ = io::stdout().lock().write_all(line);
                    }
                    OutputStream::Stderr => {
                        let _ = io::stderr().lock().write_all(line);
                    }
                }
            }
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// Stream bounded output fragments to the calling thread while both pipes are
/// drained concurrently. Lines include their original newline; long lines are
/// split to keep memory bounded. Bytes are preserved even for non-UTF-8 output
/// or an unterminated final line. No reader thread calls presentation code.
pub fn execute_command_with_observer(
    command: &CommandSpec,
    planned_executable: ExecutableIdentity,
    max_summary_bytes: usize,
    mut output: impl FnMut(OutputStream, &[u8]),
) -> ActionResult {
    let current = match executable_identity(&command.executable) {
        Ok(identity) => identity,
        Err(error) => {
            return ActionResult::validation_abort(format!(
                "cannot revalidate executable {}: {error}",
                command.executable.display()
            ));
        }
    };
    if current != planned_executable || current.resolved_kind != NodeKind::File {
        return ActionResult::validation_abort(format!(
            "provider executable changed after planning: {}",
            command.executable.display()
        ));
    }

    let mut child = match Command::new(&command.executable)
        .args(&command.args)
        .envs(&command.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return ActionResult::failed(error.to_string()),
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (sender, receiver) = crossbeam_channel::bounded(64);
    let stdout_sender = sender.clone();
    let stdout_thread = thread::spawn(move || {
        capture_stream(
            stdout,
            stdout_sender,
            OutputStream::Stdout,
            max_summary_bytes,
        )
    });
    let stderr_thread = thread::spawn(move || {
        capture_stream(stderr, sender, OutputStream::Stderr, max_summary_bytes)
    });
    for (stream, line) in receiver {
        output(stream, &line);
    }
    let status = child.wait();
    let stdout = stdout_thread
        .join()
        .unwrap_or_else(|_| b"output reader panicked".to_vec());
    let stderr = stderr_thread
        .join()
        .unwrap_or_else(|_| b"output reader panicked".to_vec());
    match status {
        Ok(status) => ActionResult {
            status: if status.success() {
                ActionStatus::Success
            } else {
                ActionStatus::Failed
            },
            removed_allocation: None,
            provider_reported: None,
            exit_code: status.code(),
            stdout_summary: summary(stdout),
            stderr_summary: summary(stderr),
            error: (!status.success()).then(|| format!("provider exited with {status}")),
        },
        Err(error) => ActionResult::failed(error.to_string()),
    }
}

fn capture_stream<R: Read>(
    mut reader: R,
    sender: crossbeam_channel::Sender<(OutputStream, Vec<u8>)>,
    stream: OutputStream,
    max_summary_bytes: usize,
) -> Vec<u8> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut pending = Vec::new();
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        for byte in &buffer[..read] {
            pending.push(*byte);
            if *byte == b'\n' || pending.len() >= 8192 {
                let _ = sender.send((stream, pending.clone()));
                pending.clear();
            }
        }
        let remaining = max_summary_bytes.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    if !pending.is_empty() {
        let _ = sender.send((stream, pending.clone()));
    }
    captured
}

fn summary(bytes: Vec<u8>) -> Option<String> {
    if bytes.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&bytes).trim().to_owned())
    }
}

#[derive(Debug)]
enum DeleteError {
    Validation(String),
    Io(io::Error),
}

impl From<io::Error> for DeleteError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(unix)]
fn direct_delete(
    path: &Path,
    children_only: bool,
    planned: CandidateIdentity,
) -> Result<(), DeleteError> {
    if !path.is_absolute() {
        return Err(DeleteError::Validation(
            "cleanup path is not absolute".into(),
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| DeleteError::Validation("cleanup target has no parent directory".into()))?;
    let name = path.file_name().ok_or_else(|| {
        DeleteError::Validation("cleanup target has no final path component".into())
    })?;
    let parent_fd = open_absolute_directory_no_follow(parent)?;
    let current = stat_at(parent_fd.as_raw_fd(), name)?;
    if current != planned {
        return Err(DeleteError::Validation(
            "target device, inode, or type changed after planning".into(),
        ));
    }
    if current.kind == NodeKind::File && !children_only {
        // The parent is held open without following links; the final name and
        // identity were checked with fstatat immediately before unlinking.
        unlink_at(parent_fd.as_raw_fd(), name, false)?;
        return Ok(());
    }
    if current.kind != NodeKind::Directory {
        return Err(DeleteError::Validation(
            "direct cleanup target is no longer a directory".into(),
        ));
    }
    let target_fd = open_directory_at(parent_fd.as_raw_fd(), name)?;
    if identity_from_fd(target_fd.as_raw_fd())? != planned {
        return Err(DeleteError::Validation(
            "target changed while it was being opened".into(),
        ));
    }
    clear_directory(target_fd.as_raw_fd())?;
    if !children_only {
        let final_identity = stat_at(parent_fd.as_raw_fd(), name)?;
        if final_identity != planned {
            return Err(DeleteError::Validation(
                "target changed before final directory removal".into(),
            ));
        }
        unlink_at(parent_fd.as_raw_fd(), name, true)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn direct_delete(
    _path: &Path,
    _children_only: bool,
    _planned: CandidateIdentity,
) -> Result<(), DeleteError> {
    Err(DeleteError::Validation(
        "direct deletion is currently supported only on Unix".into(),
    ))
}

#[cfg(unix)]
fn clear_directory(fd: RawFd) -> Result<(), DeleteError> {
    for name in list_directory(fd)? {
        let before = stat_at(fd, &name)?;
        if before.kind == NodeKind::Directory {
            let child = open_directory_at(fd, &name)?;
            if identity_from_fd(child.as_raw_fd())? != before {
                return Err(DeleteError::Validation(format!(
                    "directory entry changed during deletion: {}",
                    name.to_string_lossy()
                )));
            }
            clear_directory(child.as_raw_fd())?;
            if stat_at(fd, &name)? != before {
                return Err(DeleteError::Validation(format!(
                    "directory entry changed before removal: {}",
                    name.to_string_lossy()
                )));
            }
            unlink_at(fd, &name, true)?;
        } else {
            // Symlinks and other non-directories are removed as directory
            // entries. They are never opened or followed.
            unlink_at(fd, &name, false)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn open_absolute_directory_no_follow(path: &Path) -> Result<OwnedFd, DeleteError> {
    use std::path::Component;

    if !path.is_absolute() {
        return Err(DeleteError::Validation(
            "parent path is not absolute".into(),
        ));
    }
    let root = CString::new("/").expect("literal has no NUL");
    let fd = unsafe {
        // SAFETY: the root literal is NUL-terminated and remains alive.
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    let mut current = owned_fd(fd)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = open_directory_at(current.as_raw_fd(), name)?;
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(DeleteError::Validation(
                    "cleanup parent contains a non-normalized component".into(),
                ));
            }
        }
    }
    Ok(current)
}

#[cfg(unix)]
fn open_directory_at(parent: RawFd, name: &OsStr) -> io::Result<OwnedFd> {
    let value = CString::new(name.as_bytes())?;
    let fd = unsafe {
        // SAFETY: `parent` is an open directory and `value` is NUL-terminated.
        libc::openat(
            parent,
            value.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    owned_fd(fd)
}

#[cfg(unix)]
fn owned_fd(fd: libc::c_int) -> io::Result<OwnedFd> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: a successful `open` returns a newly owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

#[cfg(unix)]
fn list_directory(fd: RawFd) -> io::Result<Vec<OsString>> {
    let duplicate = unsafe {
        // SAFETY: `fd` is live for this call; `dup` returns independent ownership.
        libc::dup(fd)
    };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    let directory = unsafe {
        // SAFETY: `duplicate` is a directory descriptor transferred to DIR*.
        libc::fdopendir(duplicate)
    };
    if directory.is_null() {
        unsafe { libc::close(duplicate) };
        return Err(io::Error::last_os_error());
    }
    let mut names = Vec::new();
    loop {
        clear_errno();
        let entry = unsafe {
            // SAFETY: `directory` remains valid until `closedir` below.
            libc::readdir(directory)
        };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            unsafe { libc::closedir(directory) };
            return if error.raw_os_error() == Some(0) {
                Ok(names)
            } else {
                Err(error)
            };
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn clear_errno() {
    unsafe {
        // SAFETY: the platform accessor returns this thread's errno slot.
        *libc::__error() = 0;
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn clear_errno() {
    unsafe {
        // SAFETY: the platform accessor returns this thread's errno slot.
        *libc::__errno_location() = 0;
    }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "android"
    ))
))]
fn clear_errno() {}

#[cfg(unix)]
fn stat_at(parent: RawFd, name: &OsStr) -> io::Result<CandidateIdentity> {
    let value = CString::new(name.as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        // SAFETY: output points to writable storage and the name is valid.
        libc::fstatat(
            parent,
            value.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(identity_from_stat(&stat))
}

#[cfg(unix)]
fn identity_from_fd(fd: RawFd) -> io::Result<CandidateIdentity> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe { libc::fstat(fd, stat.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(identity_from_stat(&stat))
}

#[cfg(unix)]
fn identity_from_stat(stat: &libc::stat) -> CandidateIdentity {
    CandidateIdentity {
        device: stat_device(stat),
        inode: stat.st_ino,
        kind: kind_from_mode(stat_mode(stat)),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn stat_device(stat: &libc::stat) -> u64 {
    stat.st_dev
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn stat_device(stat: &libc::stat) -> u64 {
    stat.st_dev as u64
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn stat_mode(stat: &libc::stat) -> u32 {
    stat.st_mode
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn stat_mode(stat: &libc::stat) -> u32 {
    u32::from(stat.st_mode)
}

#[cfg(unix)]
fn kind_from_metadata(metadata: &fs::Metadata) -> NodeKind {
    kind_from_mode(metadata.mode())
}

#[cfg(unix)]
fn kind_from_mode(mode: u32) -> NodeKind {
    match mode & mode_type_mask() {
        value if value == regular_file_mode() => NodeKind::File,
        value if value == directory_mode() => NodeKind::Directory,
        value if value == symlink_mode() => NodeKind::Symlink,
        _ => NodeKind::Other,
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const fn mode_type_mask() -> u32 {
    libc::S_IFMT
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn mode_type_mask() -> u32 {
    u32::from(libc::S_IFMT)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const fn regular_file_mode() -> u32 {
    libc::S_IFREG
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn regular_file_mode() -> u32 {
    u32::from(libc::S_IFREG)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const fn directory_mode() -> u32 {
    libc::S_IFDIR
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn directory_mode() -> u32 {
    u32::from(libc::S_IFDIR)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const fn symlink_mode() -> u32 {
    libc::S_IFLNK
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn symlink_mode() -> u32 {
    u32::from(libc::S_IFLNK)
}

#[cfg(unix)]
fn unlink_at(parent: RawFd, name: &OsStr, directory: bool) -> io::Result<()> {
    let value = CString::new(name.as_bytes())?;
    let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
    let result = unsafe {
        // SAFETY: `parent` is live and the entry name is NUL-terminated.
        libc::unlinkat(parent, value.as_ptr(), flags)
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Render a command for review without producing a shell-evaluable string.
#[must_use]
pub fn command_argv(command: &CommandSpec) -> Vec<String> {
    std::iter::once(command.executable.display().to_string())
        .chain(command.args.iter().cloned())
        .collect()
}

/// Return only the explicitly permitted environment overrides.
#[must_use]
pub fn command_environment(command: &CommandSpec) -> BTreeMap<String, String> {
    command.environment.clone()
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use tempfile::tempdir;

    use super::*;

    fn directory_identity(path: &Path) -> CandidateIdentity {
        let metadata = fs::symlink_metadata(path).unwrap();
        CandidateIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            kind: NodeKind::Directory,
        }
    }

    #[test]
    fn descriptor_relative_delete_does_not_follow_symlinks() {
        let fixture = tempdir().unwrap();
        let scope = fixture.path().canonicalize().unwrap();
        let target = scope.join("target");
        let outside = scope.join("outside");
        fs::create_dir_all(target.join("nested")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(target.join("nested/payload"), b"generated").unwrap();
        fs::write(outside.join("must-survive"), b"user data").unwrap();
        std::os::unix::fs::symlink(&outside, target.join("escape")).unwrap();
        let result = execute_direct_delete(&target, false, directory_identity(&target), 9);
        assert_eq!(result.status, ActionStatus::Success);
        assert!(!target.exists());
        assert!(outside.join("must-survive").exists());
    }

    #[test]
    fn children_only_preserves_the_reviewed_directory() {
        let fixture = tempdir().unwrap();
        let target = fixture.path().canonicalize().unwrap().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("payload"), b"generated").unwrap();
        let result = execute_direct_delete(&target, true, directory_identity(&target), 9);
        assert_eq!(result.status, ActionStatus::Success);
        assert!(target.is_dir());
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    }

    #[test]
    fn replacement_identity_aborts_without_touching_replacement() {
        let fixture = tempdir().unwrap();
        let scope = fixture.path().canonicalize().unwrap();
        let target = scope.join("target");
        fs::create_dir(&target).unwrap();
        let planned = directory_identity(&target);
        fs::rename(&target, scope.join("old-target")).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("must-survive"), b"replacement").unwrap();
        let result = execute_direct_delete(&target, false, planned, 9);
        assert_eq!(result.status, ActionStatus::ValidationAbort);
        assert!(target.join("must-survive").exists());
    }

    #[test]
    fn executable_replacement_aborts_before_spawn() {
        let fixture = tempdir().unwrap();
        let executable = fixture.path().join("provider");
        let marker = fixture.path().join("marker");
        fs::write(&executable, format!("#!/bin/sh\ntouch {:?}\n", marker)).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let planned = executable_identity(&executable).unwrap();
        fs::rename(&executable, fixture.path().join("old-provider")).unwrap();
        fs::write(&executable, format!("#!/bin/sh\ntouch {:?}\n", marker)).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let result = execute_command(
            &CommandSpec {
                executable,
                args: Vec::new(),
                environment: BTreeMap::new(),
            },
            planned,
            false,
            1024,
        );
        assert_eq!(result.status, ActionStatus::ValidationAbort);
        assert!(!marker.exists());
    }

    #[test]
    fn symlinked_executable_entry_is_pinned_with_its_resolved_target() {
        let fixture = tempdir().unwrap();
        // Use an existing interpreter so the identity fixture does not depend
        // on writing a new executable while parallel process tests run.
        let executable = std::path::PathBuf::from("/bin/sh");
        let entry = fixture.path().join("provider");
        let marker = fixture.path().join("marker");
        std::os::unix::fs::symlink(&executable, &entry).unwrap();
        let planned = executable_identity(&entry).unwrap();
        let result = execute_command(
            &CommandSpec {
                executable: entry,
                args: vec![
                    "-c".into(),
                    ": > \"$1\"".into(),
                    "fixture".into(),
                    marker.display().to_string(),
                ],
                environment: BTreeMap::new(),
            },
            planned,
            false,
            1024,
        );
        assert_eq!(result.status, ActionStatus::Success);
        assert!(marker.exists());
    }

    #[test]
    fn symlinked_parent_component_aborts_before_traversal() {
        let fixture = tempdir().unwrap();
        let scope = fixture.path().canonicalize().unwrap();
        let real_parent = scope.join("real");
        let target = real_parent.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("must-survive"), b"generated").unwrap();
        let linked_parent = scope.join("linked");
        std::os::unix::fs::symlink(&real_parent, &linked_parent).unwrap();
        let result = execute_direct_delete(
            &linked_parent.join("target"),
            false,
            directory_identity(&target),
            9,
        );
        assert_ne!(result.status, ActionStatus::Success);
        assert!(target.join("must-survive").exists());
    }
    #[test]
    fn output_events_preserve_bytes_and_unterminated_lines_with_bounded_summaries() {
        let fixture = tempdir().unwrap();
        let executable = fixture.path().join("output-provider");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '\\377raw\\nlast'\nprintf 'err' >&2\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let command = CommandSpec {
            executable: executable.clone(),
            args: Vec::new(),
            environment: BTreeMap::new(),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = execute_command_with_observer(
            &command,
            executable_identity(&executable).unwrap(),
            4,
            |stream, line| match stream {
                OutputStream::Stdout => stdout.extend_from_slice(line),
                OutputStream::Stderr => stderr.extend_from_slice(line),
            },
        );
        assert_eq!(result.status, ActionStatus::Success);
        assert_eq!(stdout, b"\xffraw\nlast");
        assert_eq!(stderr, b"err");
        assert_eq!(result.stdout_summary.as_deref(), Some("\u{fffd}raw"));
    }
}

#[cfg(all(test, unix))]
mod provider_file_tests {
    use super::*;
    use crate::fs::{FileSystem, NativeFileSystem};

    #[test]
    fn exact_cache_file_delete_preserves_neighbors_and_refuses_replacement() {
        let fixture = tempfile::tempdir().unwrap();
        let cache = fixture.path().join("haste-map-fixture");
        let neighbor = fixture.path().join("unrelated");
        std::fs::write(&cache, b"map").unwrap();
        std::fs::write(&neighbor, b"preserve").unwrap();
        let metadata = NativeFileSystem.metadata(&cache).unwrap();
        let identity = CandidateIdentity {
            device: metadata.device,
            inode: metadata.inode,
            kind: metadata.kind,
        };
        let result = execute_direct_delete(&cache, false, identity, metadata.allocated_size);
        assert_eq!(result.status, ActionStatus::Success);
        assert!(!cache.exists());
        assert_eq!(std::fs::read(&neighbor).unwrap(), b"preserve");
        std::os::unix::fs::symlink(&neighbor, &cache).unwrap();
        assert_eq!(
            execute_direct_delete(&cache, false, identity, 0).status,
            ActionStatus::ValidationAbort
        );
        assert!(neighbor.exists());
    }
}
