//! Per-invocation metadata handed to cell handlers.
//!
//! The host passes a cell's identity (and, for commands/events, the sender's
//! identity) into each handler export as split `i64` halves of a 128-bit UUID.
//! The generated glue recombines them into a [`Metadata`] before calling the
//! user's function.

use serde::{Deserialize, Serialize};

/// The UUID identity of a cell instance.
///
/// The single shared definition lives in `myrmic-common` so the guest SDK, the
/// host, and the CLI all pass the same type. A nil `Sri` means "no cell" — e.g.
/// the sender of a message that originated outside a cell (CLI, gateway) or of
/// an `init`.
pub use myrmic_common::cells::Sri;

/// Metadata describing the context of a handler invocation.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct Metadata {
    /// The identity of the cell this handler belongs to.
    pub id: Sri,
    /// The identity of the sender of the triggering message. Nil when the
    /// message originated outside a cell, or for `init`.
    pub sender: Sri,
}

impl Metadata {
    /// Recombines the split identity halves the host passes on the Wasm ABI.
    #[must_use]
    pub fn from_parts(id_hi: i64, id_lo: i64, sender_hi: i64, sender_lo: i64) -> Self {
        Self {
            id: Sri::from_parts(id_hi, id_lo),
            sender: Sri::from_parts(sender_hi, sender_lo),
        }
    }
}
