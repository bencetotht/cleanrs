//! Incremental scan indexing is reserved for Phase 5.
//!
//! The module exists now to keep the architecture stable. No verdict or
//! reclaimability verdict is persisted here. The binary TUI owns a bounded
//! filesystem-fact restart cache; a public incremental index is still pending.

/// Current index feature availability.
pub const AVAILABLE: bool = false;
