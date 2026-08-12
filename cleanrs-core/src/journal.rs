//! Cleanup journaling is reserved for Phase 3.
//!
//! Keeping this module deliberately empty prevents read-only builds from
//! accidentally acquiring a write path.

/// Current journal feature availability.
pub const AVAILABLE: bool = false;
