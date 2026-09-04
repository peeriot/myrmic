//! Exclusive single-cell ownership of the signal layer (swarm#1340).
//!
//! The first cell to call any signal-layer host function (tap or outlet)
//! claims the whole SL for its `(Sri, Gen)` identity. Every SL call from any
//! other cell is refused with [`SL_CLAIMED`] until the owner is destroyed and
//! [`release`] frees the claim.
//!
//! This is a cell-host policy layered *above* the shared `TapClient`: because
//! only the owner ever reaches the client, the single shared UDS connection
//! (`ADR-FEAT-2026-SIG-002`) is untouched — no wire or SL-server change.

use std::sync::Mutex;

use cell_protocol::{Gen, Sri};
use myrmic_common::types::error::EACCES;

/// Return code a non-owner receives from every signal-layer host function:
/// the SL is claimed by another cell (POSIX `EACCES`). Distinct from the
/// not-found / empty codes, so a cell can tell "refused" from "no such tap".
pub(crate) const SL_CLAIMED: i32 = EACCES;

/// Identity of the cell behind a host-function call, read from the Wasmtime
/// store state so the signal-layer host functions can attribute a call.
///
/// `pub` because it bounds the generic `S` of the public `link_*` functions.
/// A state that carries no cell identity (the `()` test stub) returns `None`,
/// which disables claim enforcement — so host-function unit tests keep their
/// single-caller behaviour without threading a real identity through.
pub trait CellIdentity {
    fn sl_identity(&self) -> Option<(Sri, Gen)>;
}

impl CellIdentity for () {
    fn sl_identity(&self) -> Option<(Sri, Gen)> {
        None
    }
}

/// Gate a signal-layer host call for the caller's state: `Ok(())` if the cell
/// owns the SL (or just claimed it, or the state carries no identity),
/// `Err(SL_CLAIMED)` if another cell owns it. The single check point every
/// tap/outlet host function calls first.
pub(crate) fn gate<S: CellIdentity>(state: &S) -> Result<(), i32> {
    match state.sl_identity() {
        Some(id) if !claim_or_check(id) => Err(SL_CLAIMED),
        _ => Ok(()),
    }
}

/// The signal layer's current owner, or `None` when unclaimed. One per node
/// (this cell-host process serves every cell on the node).
static OWNER: Mutex<Option<(Sri, Gen)>> = Mutex::new(None);

/// Claim the SL for `who` if unclaimed, or confirm `who` already owns it.
///
/// Returns `true` if `who` may use the signal layer (it just claimed it, or
/// already held it), `false` if a *different* cell owns it.
pub(crate) fn claim_or_check(who: (Sri, Gen)) -> bool {
    let mut owner = OWNER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match *owner {
        None => {
            *owner = Some(who);
            true
        }
        Some(current) => current == who,
    }
}

/// Crate-facing release used by the cell teardown path: frees the SL claim if
/// `(sri, gen)` currently owns it.
pub fn release_sl_claim(sri: Sri, gen_id: Gen) {
    release((sri, gen_id));
}

/// Release the claim if `who` currently owns it (a no-op otherwise, so a
/// non-owner's teardown never frees the owner's claim). Called from the cell
/// teardown path.
pub(crate) fn release(who: (Sri, Gen)) {
    let mut owner = OWNER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *owner == Some(who) {
        *owner = None;
    }
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    *OWNER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(sri_byte: u8, gen_time: u64) -> (Sri, Gen) {
        let mut bytes = [0u8; 16];
        bytes[0] = sri_byte;
        (
            Sri::from_uuid(uuid::Uuid::from_bytes(bytes)),
            Gen::from_parts(gen_time, 1),
        )
    }

    // One sequential test: the claim state is a process-global static, so the
    // scenarios cannot run as separate parallel tests without racing on it.
    #[test]
    fn claim_lifecycle() {
        reset_for_test();
        let a = id(1, 10);
        let b = id(2, 20);

        // First accessor claims; owner may call again; others refused.
        assert!(claim_or_check(a), "first accessor claims");
        assert!(claim_or_check(a), "owner may call again");
        assert!(!claim_or_check(b), "a different cell is refused");

        // A non-owner's release must not free the owner's claim.
        release(b);
        assert!(
            !claim_or_check(b),
            "b still refused after its own no-op release"
        );

        // After the owner releases, the next cell claims.
        release(a);
        assert!(claim_or_check(b), "next cell claims after release");

        // A later incarnation of the same Sri is a distinct owner.
        reset_for_test();
        let old = id(1, 10);
        let new = id(1, 11);
        assert!(claim_or_check(old));
        assert!(
            !claim_or_check(new),
            "a new incarnation must not inherit the old one's held claim"
        );
        release(old);
        assert!(
            claim_or_check(new),
            "the new incarnation claims once the old frees it"
        );
    }
}
