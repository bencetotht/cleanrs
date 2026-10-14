//! Core filesystem and accounting primitives for `cleanrs`.
//!
//! Mutating cleanup is exposed only through the guarded Phase 3 execution API.

pub mod accounting;
pub mod execution;
pub mod fs;
pub mod index;
pub mod journal;
pub mod projects;
pub mod providers;
pub mod system_targets;
pub mod targets;

pub use accounting::{Confidence, ReclaimEstimate};
pub use fs::{DoctorReport, ScanObserver, ScanOptions, ScanProgress, ScanResult, Scanner};
