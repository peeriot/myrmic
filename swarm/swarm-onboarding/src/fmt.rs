//! Hex formatting helper for this crate's log statements.
//!
//! Everything else this crate needs - `unwrap!`, the log-level macros, the
//! assert family - comes from `defmt-or-log`, imported at the crate root.

use core::fmt::{Debug, Display, Formatter, Result};

/// Renders a byte slice as hex under both `log` and `defmt`.
///
/// `defmt` cannot format `&[u8]` the way `core` does, so the two backends need
/// separate impls; wrapping the slice is what lets one call site serve both.
pub(crate) struct Bytes<'a>(pub &'a [u8]);

impl Debug for Bytes<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        write!(f, "{:#02x?}", self.0)
    }
}

impl Display for Bytes<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        write!(f, "{:#02x?}", self.0)
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for Bytes<'_> {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(f, "{:02x}", self.0)
    }
}
