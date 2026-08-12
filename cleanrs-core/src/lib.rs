//! Core filesystem and accounting primitives for `cleanrs`.
//!
//! This crate is intentionally read-only in phases 0 and 1. It does not expose
//! deletion APIs.

pub mod accounting;
pub mod fs;
pub mod index;
pub mod journal;
pub mod providers;
pub mod targets;

pub use accounting::{Confidence, ReclaimEstimate};
pub use fs::{DoctorReport, ScanObserver, ScanOptions, ScanProgress, ScanResult, Scanner};
