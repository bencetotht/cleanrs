//! Bounded, versioned filesystem facts for repeat browser visits. No cleanup
//! selections, commands, identities authorizing mutation, or verdicts live here.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bincode::Options;
use cleanrs_core::fs::{
    Arena, FileSystem, NativeFileSystem, Node, NodeFlags, NodeIndex, NodeKind, ScanResult,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"CLNTUI01";
// Bump whenever Header, Record, or the serialized Node metadata changes.
const VERSION: u32 = 1;
const ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const CACHE_BYTES: u64 = 1024 * 1024 * 1024;
const CACHE_ENTRIES: usize = 1024;

#[derive(Clone)]
pub struct DiskCache {
    directory: PathBuf,
    gate: Arc<Mutex<()>>,
    maintained: Arc<AtomicBool>,
}
#[derive(Serialize, Deserialize)]
struct Header {
    version: u32,
    root: PathBuf,
    captured: u64,
    device: u64,
    inode: u64,
    modified: i64,
    backend: String,
}
#[derive(Serialize)]
struct BorrowedRecord<'a> {
    parent: Option<u32>,
    name: &'a str,
    node: &'a Node,
}
#[derive(Deserialize)]
struct Record {
    parent: Option<u32>,
    name: String,
    node: Node,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn codec(error: bincode::Error) -> io::Error {
    invalid(&error.to_string())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn options(limit: u64) -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(limit)
}

fn digest(reader: &mut impl Read, length: u64) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut limited = reader.take(length);
    let mut buffer = [0; 65536];
    loop {
        let count = limited.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if limited.limit() != 0 {
        return Err(invalid("truncated cache content"));
    }
    Ok(hasher.finalize().into())
}

fn verify_digest(reader: &mut BufReader<File>) -> io::Result<()> {
    let position = reader.stream_position()?;
    let length = reader
        .get_ref()
        .metadata()?
        .len()
        .checked_sub(32)
        .ok_or_else(|| invalid("missing cache checksum"))?;
    reader.seek(SeekFrom::Start(0))?;
    let calculated = digest(reader, length)?;
    let mut stored = [0; 32];
    reader.read_exact(&mut stored)?;
    if calculated != stored {
        return Err(invalid("cache checksum mismatch"));
    }
    reader.seek(SeekFrom::Start(position))?;
    Ok(())
}

impl DiskCache {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            gate: Arc::new(Mutex::new(())),
            maintained: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn for_environment(home: Option<&Path>) -> Option<Self> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.filter(|p| p.is_absolute()).map(|p| p.join(".cache")))?;
        Some(Self::new(base.join("cleanrs/tui-v1")))
    }
    fn prepare(&self) -> io::Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.directory)?;
        let metadata = fs::symlink_metadata(&self.directory)?;
        if !metadata.is_dir() {
            return Err(invalid("cache directory must not be a symlink"));
        }
        let canonical = self.directory.canonicalize()?;
        if canonical
            .components()
            .any(|p| p.as_os_str() == "CloudStorage" || p.as_os_str() == "Mobile Documents")
            || NativeFileSystem
                .metadata(&canonical)?
                .flags
                .contains(NodeFlags::DATALESS)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "browser cache requires local, unsynced storage",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // Cache corruption must never provide a path escape or a mutation
            // authorization. Owner-only files also protect private path names.
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(invalid("cache directory has a different owner"));
            }
            fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))?;
        }
        if !self.maintained.swap(true, Ordering::AcqRel) {
            if let Err(error) = self.remove_orphaned_writes() {
                self.maintained.store(false, Ordering::Release);
                return Err(error);
            }
        }
        Ok(())
    }
    fn remove_orphaned_writes(&self) -> io::Result<()> {
        // Quitting during serialization can end the process before tempfile's
        // destructor runs. Reclaim those files on the next cache visit, while
        // preserving writes owned by another live TUI process.
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str().and_then(|n| n.strip_prefix(".write-")) else {
                continue;
            };
            if !entry.file_type()?.is_file() {
                continue;
            }
            let old = entry.metadata()?.modified()?.elapsed().unwrap_or_default()
                > Duration::from_secs(86400);
            #[cfg(unix)]
            let dead = name
                .split('-')
                .next()
                .and_then(|n| n.parse::<i32>().ok())
                .filter(|p| *p > 0)
                .is_some_and(|pid| {
                    // Signal zero checks liveness without sending a signal.
                    (unsafe { libc::kill(pid, 0) }) != 0
                        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                });
            #[cfg(not(unix))]
            let dead = {
                let _ = name;
                false
            };
            if old || dead {
                Self::local_file(&entry.path())?;
                match fs::remove_file(entry.path()) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }
    fn entry(&self, path: &Path) -> PathBuf {
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            path.as_os_str().as_bytes()
        };
        #[cfg(not(unix))]
        let text = path.to_string_lossy();
        #[cfg(not(unix))]
        let bytes = text.as_bytes();
        self.directory
            .join(format!("{:x}.scan", Sha256::digest(bytes)))
    }
    fn local_file(path: &Path) -> io::Result<()> {
        if NativeFileSystem
            .metadata(path)?
            .flags
            .contains(NodeFlags::DATALESS)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "dataless cache content is never read or deleted",
            ));
        }
        Ok(())
    }
    fn reader(path: &Path) -> io::Result<(BufReader<File>, Header)> {
        Self::local_file(path)?;
        let mut open = OpenOptions::new();
        open.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.custom_flags(libc::O_NOFOLLOW);
        }
        let file = open.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > ENTRY_BYTES {
            return Err(invalid("invalid cache entry size/type"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                return Err(invalid("cache entry is not owner-only"));
            }
        }
        let mut reader = BufReader::new(file);
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("cache format mismatch"));
        }
        let header: Header = options(8192).deserialize_from(&mut reader).map_err(codec)?;
        if header.version != VERSION || !header.root.is_absolute() || header.captured > now() {
            return Err(invalid("cache version/path/time mismatch"));
        }
        Ok((reader, header))
    }
    pub fn load(&self, path: &Path) -> io::Result<Option<(ScanResult, Duration)>> {
        // An absent cache is a normal miss. An unreadable or corrupt entry also
        // falls back to scanning at the caller, and never to a cached verdict.
        if !self.directory.exists() {
            return Ok(None);
        }
        self.prepare()?;
        let entry = self.entry(path);
        let (mut reader, header) = match Self::reader(&entry) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                if error.kind() == io::ErrorKind::InvalidData {
                    let _ = fs::remove_file(entry);
                }
                return Err(error);
            }
        };
        let root = NativeFileSystem.metadata(path)?;
        if header.root != path
            || (root.device, root.inode, root.modified_unix_seconds)
                != (header.device, header.inode, header.modified)
            || root.kind != NodeKind::Directory
            || root
                .flags
                .intersects(NodeFlags::DATALESS | NodeFlags::FIRMLINK)
        {
            return Ok(None);
        }
        // Check integrity before any cached metadata reaches accounting. Header
        // checks stay cheap, so changed roots do not require hashing old arenas.
        verify_digest(&mut reader)?;
        let count: u64 = options(8).deserialize_from(&mut reader).map_err(codec)?;
        if count == 0 || count > ENTRY_BYTES / 64 {
            return Err(invalid("invalid cache node count"));
        }
        let mut arena = Arena::new(path);
        for position in 0..count {
            // Bound each record before allocating a corrupt filename. Validate
            // forward-only parents so cycles and '..' path escapes are impossible.
            let record: Record = options(4096).deserialize_from(&mut reader).map_err(codec)?;
            if position == 0 {
                if record.parent.is_some()
                    || record.node.kind != NodeKind::Directory
                    || (record.node.device, record.node.inode) != (root.device, root.inode)
                {
                    return Err(invalid("invalid cached root"));
                }
                arena.update_metadata(arena.root(), record.node);
            } else {
                let parent = record
                    .parent
                    .filter(|p| (*p as u64) < position)
                    .ok_or_else(|| invalid("invalid cached parent"))?;
                if record.name.is_empty()
                    || record.name == "."
                    || record.name == ".."
                    || record.name.contains(['/', '\0'])
                    || arena.nodes[parent as usize].kind != NodeKind::Directory
                {
                    return Err(invalid("invalid cached filename/parent kind"));
                }
                arena.push_child(
                    NodeIndex::from_usize(parent as usize),
                    &record.name,
                    record.node,
                );
            }
        }
        let mut checksum = [0; 32];
        reader.read_exact(&mut checksum)?;
        let mut trailing = [0];
        if reader.read(&mut trailing)? != 0 {
            return Err(invalid("trailing cache data"));
        }
        let dataless_entries = arena
            .nodes
            .iter()
            .filter(|n| n.flags.contains(NodeFlags::DATALESS))
            .count();
        let mount_boundaries = arena
            .nodes
            .iter()
            .filter(|n| n.flags.contains(NodeFlags::MOUNT_BOUNDARY))
            .count();
        Ok(Some((
            ScanResult {
                root: path.into(),
                arena,
                coverage_errors: Vec::new(),
                backend: header.backend,
                dataless_entries,
                mount_boundaries,
            },
            Duration::from_secs(now().saturating_sub(header.captured)),
        )))
    }
    /// A folder initially opened as the browser root has per-child entries.
    /// Recompose those when it later becomes a child of an unvisited parent,
    /// instead of recursively scanning the already measured folder again.
    pub fn load_tree(&self, path: &Path) -> io::Result<Option<(ScanResult, Duration)>> {
        if let Some(scan) = self.load(path)? {
            return Ok(Some(scan));
        }
        let filesystem = NativeFileSystem;
        let root = filesystem.metadata(path)?;
        if root.kind != NodeKind::Directory
            || root
                .flags
                .intersects(NodeFlags::DATALESS | NodeFlags::FIRMLINK)
        {
            return Ok(None);
        }
        let entries = filesystem.read_dir(path)?;
        // Check small headers first. A missing sibling must not make us decode
        // all other large arenas only to discard them and load them a second time.
        for entry in &entries {
            let m = &entry.metadata;
            if m.kind == NodeKind::Directory
                && m.device == root.device
                && !m.flags.intersects(
                    NodeFlags::DATALESS | NodeFlags::FIRMLINK | NodeFlags::MOUNT_BOUNDARY,
                )
            {
                let child = path.join(&entry.name);
                let Ok((_, h)) = Self::reader(&self.entry(&child)) else {
                    return Ok(None);
                };
                if h.root != child
                    || (h.device, h.inode, h.modified)
                        != (m.device, m.inode, m.modified_unix_seconds)
                {
                    return Ok(None);
                }
            }
        }
        let mut children = Vec::new();
        let mut age = Duration::ZERO;
        for entry in &entries {
            let m = &entry.metadata;
            if m.kind == NodeKind::Directory
                && m.device == root.device
                && !m.flags.intersects(
                    NodeFlags::DATALESS | NodeFlags::FIRMLINK | NodeFlags::MOUNT_BOUNDARY,
                )
            {
                let Some((scan, captured_age)) = self.load(&path.join(&entry.name))? else {
                    return Ok(None);
                };
                age = age.max(captured_age);
                children.push((entry.name.clone(), scan));
            }
        }
        if children.is_empty() {
            return Ok(None);
        }
        let mut arena = Arena::new(path);
        arena.update_metadata(arena.root(), crate::pipeline::metadata_node(&root));
        for entry in entries {
            let mut node = crate::pipeline::metadata_node(&entry.metadata);
            if node.device != root.device {
                node.flags.insert(NodeFlags::MOUNT_BOUNDARY);
            }
            let index = arena.push_child(arena.root(), &entry.name.to_string_lossy(), node);
            if let Some((_, scan)) = children.iter().find(|(name, _)| name == &entry.name) {
                scan.arena
                    .copy_children_into(scan.arena.root(), &mut arena, index);
            }
        }
        let dataless_entries = arena
            .nodes
            .iter()
            .filter(|n| n.flags.contains(NodeFlags::DATALESS))
            .count();
        let mount_boundaries = arena
            .nodes
            .iter()
            .filter(|n| n.flags.contains(NodeFlags::MOUNT_BOUNDARY))
            .count();
        Ok(Some((
            ScanResult {
                root: path.into(),
                arena,
                coverage_errors: Vec::new(),
                backend: "composed disk cache".into(),
                dataless_entries,
                mount_boundaries,
            },
            age,
        )))
    }

    #[cfg(test)]
    pub fn save(&self, scan: &ScanResult) -> io::Result<()> {
        self.save_if(scan, || true)
    }
    pub fn save_if(&self, scan: &ScanResult, current: impl FnOnce() -> bool) -> io::Result<()> {
        // Serialize publication with invalidation, and check cancellation under
        // the same lock so an older scan cannot resurrect an explicitly refreshed entry.
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("cache lock poisoned"))?;
        if !current() {
            return Ok(());
        }
        if !scan.coverage_complete() {
            return Ok(());
        }
        self.prepare()?;
        let root = &scan.arena.nodes[0];
        let header = Header {
            version: VERSION,
            root: scan.root.clone(),
            captured: now(),
            device: root.device,
            inode: root.inode,
            modified: root.modified_unix_seconds,
            backend: scan.backend.clone(),
        };
        let mut temporary = tempfile::Builder::new()
            .prefix(&format!(".write-{}-", std::process::id()))
            .tempfile_in(&self.directory)?;
        {
            let mut writer = BufWriter::new(temporary.as_file_mut());
            writer.write_all(MAGIC)?;
            options(8192)
                .serialize_into(&mut writer, &header)
                .map_err(codec)?;
            options(8)
                .serialize_into(&mut writer, &(scan.arena.nodes.len() as u64))
                .map_err(codec)?;
            let mut bytes = 16 + options(8192).serialized_size(&header).map_err(codec)?;
            for (position, node) in scan.arena.nodes.iter().enumerate() {
                let index = NodeIndex::from_usize(position);
                let record = BorrowedRecord {
                    parent: scan.arena.parent(index).map(|p| p.as_usize() as u32),
                    name: if position == 0 {
                        ""
                    } else {
                        scan.arena.name(index)
                    },
                    node,
                };
                bytes =
                    bytes.saturating_add(options(4096).serialized_size(&record).map_err(codec)?);
                if bytes > ENTRY_BYTES - 32 {
                    return Err(invalid("cache entry exceeds 256 MiB"));
                }
                options(4096)
                    .serialize_into(&mut writer, &record)
                    .map_err(codec)?;
            }
            writer.flush()?;
        }
        if temporary.as_file().metadata()?.len() > ENTRY_BYTES - 32 {
            return Err(invalid("cache entry exceeds 256 MiB"));
        }
        let length = temporary.as_file().metadata()?.len();
        temporary.as_file_mut().seek(SeekFrom::Start(0))?;
        let checksum = digest(temporary.as_file_mut(), length)?;
        temporary.as_file_mut().write_all(&checksum)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.entry(&scan.root))
            .map_err(|e| e.error)?;
        self.prune(CACHE_BYTES, CACHE_ENTRIES)
    }
    fn entries(&self) -> io::Result<Vec<(PathBuf, u64, SystemTime)>> {
        if !self.directory.exists() {
            return Ok(Vec::new());
        }
        fs::read_dir(&self.directory)?
            .filter_map(|e| match e {
                Ok(entry) if entry.path().extension().is_some_and(|e| e == "scan") => Some(
                    entry
                        .metadata()
                        .map(|m| (entry.path(), m.len(), m.modified().unwrap_or(UNIX_EPOCH))),
                ),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }
    fn prune(&self, bytes_limit: u64, entry_limit: usize) -> io::Result<()> {
        let mut entries = self.entries()?;
        entries.sort_by_key(|e| e.2);
        let mut bytes: u64 = entries.iter().map(|e| e.1).sum();
        let mut count = entries.len();
        for (path, size, _) in entries {
            if bytes <= bytes_limit && count <= entry_limit {
                break;
            }
            Self::local_file(&path)?;
            match fs::remove_file(path) {
                Ok(()) => {
                    bytes = bytes.saturating_sub(size);
                    count -= 1;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    bytes = bytes.saturating_sub(size);
                    count -= 1;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    pub fn invalidate(&self, scope: &Path) -> io::Result<()> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| invalid("cache lock poisoned"))?;
        if !self.directory.exists() {
            return Ok(());
        }
        self.prepare()?;
        for (path, _, _) in self.entries()? {
            let remove = Self::reader(&path).map_or(true, |(_, h)| {
                h.root.starts_with(scope) || scope.starts_with(&h.root)
            });
            if remove {
                Self::local_file(&path)?;
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleanrs_core::{ScanOptions, Scanner};
    fn fixture() -> (tempfile::TempDir, DiskCache, ScanResult) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tree");
        fs::create_dir_all(root.join("deep")).unwrap();
        fs::write(root.join("deep/a"), vec![7; 8192]).unwrap();
        fs::hard_link(root.join("deep/a"), root.join("linked")).unwrap();
        let root = root.canonicalize().unwrap();
        let scan = Scanner::new(ScanOptions::default()).scan(&root).unwrap();
        let cache = DiskCache::new(temp.path().join("cache"));
        (temp, cache, scan)
    }
    #[test]
    fn round_trip_preserves_arena_accounting_and_private_permissions() {
        let (_temp, cache, scan) = fixture();
        cache.save(&scan).unwrap();
        let (restored, _) = cache.load(&scan.root).unwrap().unwrap();
        assert_eq!(
            crate::subtree_sizes(&scan.arena, scan.arena.root()),
            crate::subtree_sizes(&restored.arena, restored.arena.root())
        );
        assert_eq!(scan.arena.nodes.len(), restored.arena.nodes.len());
        for n in 0..scan.arena.nodes.len() {
            let i = NodeIndex::from_usize(n);
            assert_eq!(scan.arena.path(i), restored.arena.path(i));
            assert_eq!(
                serde_json::to_value(&scan.arena.nodes[n]).unwrap(),
                serde_json::to_value(&restored.arena.nodes[n]).unwrap()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&cache.directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(cache.entry(&scan.root))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn corruption_replacement_invalidation_and_limits_fail_closed() {
        let (temp, cache, scan) = fixture();
        cache.save(&scan).unwrap();
        fs::write(cache.entry(&scan.root), b"broken").unwrap();
        assert!(cache.load(&scan.root).is_err());
        cache.save(&scan).unwrap();
        let entry = cache.entry(&scan.root);
        let length = fs::metadata(&entry).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&entry)
            .unwrap()
            .set_len(length - 1)
            .unwrap();
        assert!(
            cache.load(&scan.root).is_err(),
            "truncated records must be discarded"
        );
        cache.save(&scan).unwrap();
        let bytes = fs::read(&entry).unwrap();
        let mut cursor = io::Cursor::new(&bytes[8..]);
        let mut header: Header = options(8192).deserialize_from(&mut cursor).unwrap();
        let consumed = cursor.position() as usize + 8;
        header.version += 1;
        let mut encoded = MAGIC.to_vec();
        options(8192).serialize_into(&mut encoded, &header).unwrap();
        encoded.extend_from_slice(&bytes[consumed..]);
        fs::write(&entry, encoded).unwrap();
        assert!(
            cache.load(&scan.root).is_err(),
            "unsupported schema versions cannot be read"
        );
        cache.save(&scan).unwrap();
        let mut damaged = fs::read(cache.entry(&scan.root)).unwrap();
        let index = damaged.len() - 40;
        damaged[index] ^= 1;
        fs::write(cache.entry(&scan.root), damaged).unwrap();
        assert!(
            cache
                .load(&scan.root)
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
        cache.save(&scan).unwrap();
        // Keep the old inode alive so replacement cannot accidentally reuse it.
        fs::rename(&scan.root, temp.path().join("old-tree")).unwrap();
        fs::create_dir(&scan.root).unwrap();
        assert!(cache.load(&scan.root).unwrap().is_none());
        cache.invalidate(&scan.root).unwrap();
        assert!(cache.entries().unwrap().is_empty());
        let fresh = Scanner::new(ScanOptions::default())
            .scan(&scan.root)
            .unwrap();
        cache.save_if(&fresh, || false).unwrap();
        assert!(
            cache.entries().unwrap().is_empty(),
            "cancelled scans cannot resurrect invalidated entries"
        );
        cache.save(&fresh).unwrap();
        cache.prune(0, 0).unwrap();
        assert!(cache.entries().unwrap().is_empty());
        cache.save(&fresh).unwrap();
        OpenOptions::new()
            .write(true)
            .open(cache.entry(&fresh.root))
            .unwrap()
            .set_len(ENTRY_BYTES + 1)
            .unwrap();
        assert!(cache.load(&fresh.root).is_err());
    }
    #[test]
    fn stored_age_is_preserved_and_structural_path_escapes_are_rejected() {
        let (_temp, cache, scan) = fixture();
        cache.save(&scan).unwrap();
        let bytes = fs::read(cache.entry(&scan.root)).unwrap();
        let mut cursor = io::Cursor::new(&bytes[8..]);
        let mut header: Header = options(8192).deserialize_from(&mut cursor).unwrap();
        let consumed = cursor.position() as usize + 8;
        header.captured = now().saturating_sub(400 * 86400);
        let mut encoded = MAGIC.to_vec();
        options(8192).serialize_into(&mut encoded, &header).unwrap();
        encoded.extend_from_slice(&bytes[consumed..bytes.len() - 32]);
        let checksum = Sha256::digest(&encoded);
        encoded.extend_from_slice(&checksum);
        fs::write(cache.entry(&scan.root), encoded).unwrap();
        assert!(cache.load(&scan.root).unwrap().unwrap().1 >= Duration::from_secs(400 * 86400));
        for (parent, name) in [
            (Some(0), ".."),
            (Some(2), "forward-parent"),
            (Some(0), "/absolute"),
        ] {
            let mut encoded = MAGIC.to_vec();
            options(8192).serialize_into(&mut encoded, &header).unwrap();
            options(8).serialize_into(&mut encoded, &2_u64).unwrap();
            options(4096)
                .serialize_into(
                    &mut encoded,
                    &BorrowedRecord {
                        parent: None,
                        name: "",
                        node: &scan.arena.nodes[0],
                    },
                )
                .unwrap();
            options(4096)
                .serialize_into(
                    &mut encoded,
                    &BorrowedRecord {
                        parent,
                        name,
                        node: &scan.arena.nodes[1],
                    },
                )
                .unwrap();
            let checksum = Sha256::digest(&encoded);
            encoded.extend_from_slice(&checksum);
            fs::write(cache.entry(&scan.root), encoded).unwrap();
            assert!(cache.load(&scan.root).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn startup_reclaims_dead_writers_but_preserves_live_writer_files() {
        let (_temp, cache, _scan) = fixture();
        cache.prepare().unwrap();
        let orphan = cache.directory.join(".write-2147483647-orphan");
        let live = cache
            .directory
            .join(format!(".write-{}-live", std::process::id()));
        fs::write(&orphan, b"fixture").unwrap();
        fs::write(&live, b"fixture").unwrap();
        let reopened = DiskCache::new(cache.directory.clone());
        reopened.prepare().unwrap();
        assert!(!orphan.exists());
        assert!(live.exists());
    }
    #[test]
    fn cloud_namespace_cache_refusal_preserves_existing_content() {
        let (temp, _cache, scan) = fixture();
        let directory = temp.path().join("CloudStorage/cache");
        fs::create_dir_all(&directory).unwrap();
        let protected = directory.join("keep.scan");
        fs::write(&protected, b"fixture cloud content").unwrap();
        let cache = DiskCache::new(directory);
        assert!(cache.save(&scan).is_err());
        assert!(cache.invalidate(&scan.root).is_err());
        assert_eq!(fs::read(protected).unwrap(), b"fixture cloud content");
    }
}
