//! Runtime report mode.
//!
//! Enabled via `--features report`. Emits structured heap-stats snapshots at
//! key firmware lifecycle milestones over the standard log channel (INFO level).
//!
//! Output format:
//! ```text
//! [report] <label> | total: <N> B | used: <N> B | free: <N> B
//! ```

/// Emit a heap-stats snapshot tagged with `label`.
///
/// Compiled away completely when the `report` feature is disabled.
#[inline]
pub fn snapshot(label: &str) {
    #[cfg(feature = "report")]
    {
        let s = esp_alloc::HEAP.stats();
        log::info!(
            "[report] {} | total: {} B | used: {} B | free: {} B",
            label,
            s.size,
            s.current_usage,
            s.size.saturating_sub(s.current_usage),
        );
    }
    #[cfg(not(feature = "report"))]
    {
        let _ = label;
    }
}
