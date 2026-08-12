//! Filesystem abstraction, arena tree, traversal, and volume diagnostics.

mod arena;
mod doctor;
mod platform;
mod scanner;

pub use arena::{Arena, Node, NodeFlags, NodeIndex, NodeKind};
pub use doctor::{DoctorReport, VolumeReport, doctor};
pub use platform::{DirEntry, EntryMetadata, FileSystem, NativeFileSystem};
pub use scanner::{CoverageError, ScanObserver, ScanOptions, ScanProgress, ScanResult, Scanner};
