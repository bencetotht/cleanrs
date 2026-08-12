use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
#[cfg(not(unix))]
use std::time::UNIX_EPOCH;

use super::{NodeFlags, NodeKind};

/// Metadata collected without following symlinks.
#[derive(Debug, Clone)]
pub struct EntryMetadata {
    pub kind: NodeKind,
    pub apparent_size: u64,
    pub allocated_size: u64,
    pub private_size: Option<u64>,
    pub device: u64,
    pub inode: u64,
    pub link_count: u64,
    pub clone_id: Option<u64>,
    pub clone_refcount: Option<u32>,
    pub modified_unix_seconds: i64,
    pub flags: NodeFlags,
}

/// One name and its already-collected metadata.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: OsString,
    pub metadata: EntryMetadata,
}

/// Platform seam for filesystem metadata and batched directory enumeration.
pub trait FileSystem: Send + Sync + 'static {
    /// Metadata for a single path without following symbolic links.
    fn metadata(&self, path: &Path) -> io::Result<EntryMetadata>;

    /// Enumerate a directory. Native implementations may batch attributes.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;

    /// Human-readable backend label for diagnostics.
    fn backend_name(&self) -> &'static str;
}

/// Native backend: `getattrlistbulk` on macOS, safe portable fallback elsewhere.
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeFileSystem;

impl FileSystem for NativeFileSystem {
    fn metadata(&self, path: &Path) -> io::Result<EntryMetadata> {
        portable_metadata(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        #[cfg(target_os = "macos")]
        {
            match super::platform::macos::read_dir_bulk(path) {
                Ok(entries) => return Ok(entries),
                Err(error) => {
                    tracing::debug!(%error, path = %path.display(), "bulk metadata unavailable; falling back")
                }
            }
        }
        read_dir_portable(path)
    }

    fn backend_name(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "getattrlistbulk (per-directory fallback enabled)"
        } else {
            "readdir + lstat fallback"
        }
    }
}

fn read_dir_portable(path: &Path) -> io::Result<Vec<DirEntry>> {
    fs::read_dir(path)?
        .map(|result| {
            let entry = result?;
            let mut metadata = portable_metadata(&entry.path())?;
            metadata.flags.insert(NodeFlags::METADATA_FALLBACK);
            Ok(DirEntry {
                name: entry.file_name(),
                metadata,
            })
        })
        .collect()
}

fn portable_metadata(path: &Path) -> io::Result<EntryMetadata> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    let kind = if file_type.is_file() {
        NodeKind::File
    } else if file_type.is_dir() {
        NodeKind::Directory
    } else if file_type.is_symlink() {
        NodeKind::Symlink
    } else {
        NodeKind::Other
    };

    #[cfg(unix)]
    {
        #[cfg(target_os = "macos")]
        use std::os::darwin::fs::MetadataExt as DarwinMetadataExt;
        use std::os::unix::fs::MetadataExt;
        #[cfg(target_os = "macos")]
        let flags = flags_from_raw(DarwinMetadataExt::st_flags(&metadata));
        #[cfg(not(target_os = "macos"))]
        let flags = NodeFlags::empty();
        Ok(EntryMetadata {
            kind,
            apparent_size: metadata.size(),
            allocated_size: metadata.blocks().saturating_mul(512),
            // Portable stat cannot expose APFS private allocation. On a
            // non-sharing filesystem, allocated is a useful exact fallback;
            // macOS leaves this unavailable when bulk extended attrs fail.
            private_size: (!cfg!(target_os = "macos"))
                .then_some(metadata.blocks().saturating_mul(512)),
            device: metadata.dev(),
            inode: metadata.ino(),
            link_count: metadata.nlink(),
            clone_id: None,
            clone_refcount: None,
            modified_unix_seconds: metadata.mtime(),
            flags,
        })
    }

    #[cfg(not(unix))]
    {
        let modified_unix_seconds = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| {
                i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
            });
        Ok(EntryMetadata {
            kind,
            apparent_size: metadata.len(),
            allocated_size: metadata.len(),
            private_size: Some(metadata.len()),
            device: 0,
            inode: 0,
            link_count: 1,
            clone_id: None,
            clone_refcount: None,
            modified_unix_seconds,
            flags: NodeFlags::METADATA_FALLBACK,
        })
    }
}

#[cfg(target_os = "macos")]
fn flags_from_raw(raw: u32) -> NodeFlags {
    let mut flags = NodeFlags::empty();
    const SF_FIRMLINK: u32 = 0x0080_0000;
    const SF_DATALESS: u32 = 0x4000_0000;
    if raw & SF_FIRMLINK != 0 {
        flags.insert(NodeFlags::FIRMLINK);
    }
    if raw & SF_DATALESS != 0 {
        flags.insert(NodeFlags::DATALESS);
    }
    flags
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, CString, OsString};
    use std::io;
    use std::mem::size_of;
    use std::os::fd::RawFd;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;

    use super::{DirEntry, EntryMetadata};
    use crate::fs::{NodeFlags, NodeKind};

    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_DEVID: u32 = 0x0000_0002;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
    const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
    const ATTR_CMN_FILEID: u32 = 0x0200_0000;
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    const ATTR_FILE_TOTALSIZE: u32 = 0x0000_0002;
    const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
    const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
    const ATTR_CMNEXT_PRIVATESIZE: u32 = 0x0000_0008;
    const ATTR_CMNEXT_CLONEID: u32 = 0x0000_0100;
    const ATTR_CMNEXT_CLONE_REFCNT: u32 = 0x0000_1000;
    const FSOPT_ATTR_CMN_EXTENDED: u64 = 0x0000_0020;
    const FSOPT_PACK_INVAL_ATTRS: u64 = 0x0000_0008;
    const SF_FIRMLINK: u32 = 0x0080_0000;
    const SF_DATALESS: u32 = 0x4000_0000;

    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    #[derive(Clone, Copy)]
    #[repr(C)]
    struct AttributeSet {
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    #[derive(Clone, Copy)]
    #[repr(C)]
    struct AttrReference {
        offset: i32,
        length: u32,
    }

    unsafe extern "C" {
        fn getattrlistbulk(
            dirfd: RawFd,
            attrlist: *mut AttrList,
            attrbuf: *mut libc::c_void,
            attrbufsize: libc::size_t,
            options: u64,
        ) -> libc::c_int;
    }

    pub(super) fn read_dir_bulk(path: &Path) -> io::Result<Vec<DirEntry>> {
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // O_DIRECTORY plus O_CLOEXEC keeps this descriptor scoped to metadata
        // enumeration and prevents accidental inheritance by provider commands.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let _guard = FdGuard(fd);

        let mut attrs = AttrList {
            bitmapcount: 5,
            reserved: 0,
            commonattr: ATTR_CMN_NAME
                | ATTR_CMN_DEVID
                | ATTR_CMN_OBJTYPE
                | ATTR_CMN_MODTIME
                | ATTR_CMN_FLAGS
                | ATTR_CMN_FILEID
                | ATTR_CMN_ERROR
                | ATTR_CMN_RETURNED_ATTRS,
            volattr: 0,
            dirattr: 0,
            fileattr: ATTR_FILE_LINKCOUNT | ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE,
            forkattr: ATTR_CMNEXT_PRIVATESIZE | ATTR_CMNEXT_CLONEID | ATTR_CMNEXT_CLONE_REFCNT,
        };
        let mut buffer = vec![0_u8; 256 * 1024];
        let mut output = Vec::new();

        loop {
            let count = unsafe {
                getattrlistbulk(
                    fd,
                    &raw mut attrs,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    FSOPT_ATTR_CMN_EXTENDED | FSOPT_PACK_INVAL_ATTRS,
                )
            };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                break;
            }
            parse_records(&buffer, count as usize, &mut output)?;
        }
        Ok(output)
    }

    fn parse_records(buffer: &[u8], count: usize, output: &mut Vec<DirEntry>) -> io::Result<()> {
        let mut record_start = 0_usize;
        for _ in 0..count {
            let length = read::<u32>(buffer, record_start)? as usize;
            if length < size_of::<u32>() || record_start.saturating_add(length) > buffer.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid bulk record length",
                ));
            }
            let record = &buffer[record_start..record_start + length];
            output.push(parse_record(record)?);
            record_start += length;
        }
        Ok(())
    }

    fn parse_record(record: &[u8]) -> io::Result<DirEntry> {
        let mut cursor = 4_usize;
        let returned = take::<AttributeSet>(record, &mut cursor)?;
        let entry_error = if returned.commonattr & ATTR_CMN_ERROR != 0 {
            take::<u32>(record, &mut cursor)?
        } else {
            0
        };
        if entry_error != 0 {
            let error_code = i32::from_ne_bytes(entry_error.to_ne_bytes());
            return Err(io::Error::from_raw_os_error(error_code));
        }

        let name = if returned.commonattr & ATTR_CMN_NAME != 0 {
            let reference_position = cursor;
            let reference = take::<AttrReference>(record, &mut cursor)?;
            let start = reference_position
                .checked_add_signed(reference.offset as isize)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid name offset"))?;
            let end = start.saturating_add(reference.length as usize);
            let bytes = record.get(start..end).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "name outside bulk record")
            })?;
            let c_name = CStr::from_bytes_until_nul(bytes).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "name is not terminated")
            })?;
            OsString::from_vec(c_name.to_bytes().to_vec())
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bulk record omitted name",
            ));
        };

        let device = take_if::<i32>(record, &mut cursor, returned.commonattr, ATTR_CMN_DEVID)?
            .map_or(0, |value| value as u32 as u64);
        let raw_kind = take_if::<u32>(record, &mut cursor, returned.commonattr, ATTR_CMN_OBJTYPE)?
            .unwrap_or_default();
        let modified =
            take_if::<libc::timespec>(record, &mut cursor, returned.commonattr, ATTR_CMN_MODTIME)?
                .map_or(0, |value| value.tv_sec);
        let raw_flags = take_if::<u32>(record, &mut cursor, returned.commonattr, ATTR_CMN_FLAGS)?
            .unwrap_or_default();
        let inode = take_if::<u64>(record, &mut cursor, returned.commonattr, ATTR_CMN_FILEID)?
            .unwrap_or_default();
        let link_count =
            take_if::<u32>(record, &mut cursor, returned.fileattr, ATTR_FILE_LINKCOUNT)?
                .map_or(1, u64::from);
        let apparent_size =
            take_if::<i64>(record, &mut cursor, returned.fileattr, ATTR_FILE_TOTALSIZE)?
                .unwrap_or_default()
                .max(0) as u64;
        let allocated_size =
            take_if::<i64>(record, &mut cursor, returned.fileattr, ATTR_FILE_ALLOCSIZE)?
                .unwrap_or_default()
                .max(0) as u64;
        let private_size = take_if::<i64>(
            record,
            &mut cursor,
            returned.forkattr,
            ATTR_CMNEXT_PRIVATESIZE,
        )?
        .map(|value| value.max(0) as u64);
        let clone_id = take_if::<u64>(record, &mut cursor, returned.forkattr, ATTR_CMNEXT_CLONEID)?;
        let clone_refcount = take_if::<u32>(
            record,
            &mut cursor,
            returned.forkattr,
            ATTR_CMNEXT_CLONE_REFCNT,
        )?;

        let mut flags = NodeFlags::empty();
        if raw_flags & SF_FIRMLINK != 0 {
            flags.insert(NodeFlags::FIRMLINK);
        }
        if raw_flags & SF_DATALESS != 0 {
            flags.insert(NodeFlags::DATALESS);
        }
        let kind = match raw_kind {
            1 => NodeKind::File,
            2 => NodeKind::Directory,
            5 => NodeKind::Symlink,
            _ => NodeKind::Other,
        };
        Ok(DirEntry {
            name,
            metadata: EntryMetadata {
                kind,
                apparent_size,
                allocated_size,
                private_size,
                device,
                inode,
                link_count,
                clone_id,
                clone_refcount,
                modified_unix_seconds: modified,
                flags,
            },
        })
    }

    fn take_if<T: Copy>(
        bytes: &[u8],
        cursor: &mut usize,
        returned: u32,
        bit: u32,
    ) -> io::Result<Option<T>> {
        if returned & bit == 0 {
            Ok(None)
        } else {
            take(bytes, cursor).map(Some)
        }
    }

    fn take<T: Copy>(bytes: &[u8], cursor: &mut usize) -> io::Result<T> {
        let value = read(bytes, *cursor)?;
        *cursor = cursor.saturating_add(size_of::<T>());
        Ok(value)
    }

    fn read<T: Copy>(bytes: &[u8], offset: usize) -> io::Result<T> {
        let end = offset.saturating_add(size_of::<T>());
        if end > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated bulk record",
            ));
        }
        Ok(unsafe { bytes.as_ptr().add(offset).cast::<T>().read_unaligned() })
    }

    struct FdGuard(RawFd);
    impl Drop for FdGuard {
        fn drop(&mut self) {
            unsafe { libc::close(self.0) };
        }
    }
}
