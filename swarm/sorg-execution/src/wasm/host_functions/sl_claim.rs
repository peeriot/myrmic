//! Exclusive single-cell ownership of the signal layer.
//!
//! The first cell to write an outlet claims the SL for its `(Sri, Gen)`
//! identity. Outlet writes from any other cell are refused with [`SL_CLAIMED`]
//! until the owner is destroyed and [`release`] frees the claim, or a strictly
//! newer `Gen` of the owner's `Sri` takes it over. Taps and outlet lookups are
//! not gated. Each refused identity is logged once per owner.
//!
//! This is a cell-host policy layered *above* the shared `TapClient`: only
//! outlet writes are owner-only, and the single shared UDS connection is
//! untouched - no wire or SL-server change.

use std::sync::Mutex;

use cell_protocol::{Gen, Sri};
use myrmic_common::types::error::EACCES;

/// Return code a non-owner receives from an outlet write: the SL is claimed by
/// another cell (POSIX `EACCES`). Distinct from the not-found / empty codes,
/// so a cell can tell "refused" from "no such outlet".
pub(crate) const SL_CLAIMED: i32 = EACCES;

/// Identity of the cell behind a host-function call, read from the Wasmtime
/// store state so the outlet-write gate can attribute a call.
///
/// `pub` because it bounds the generic `S` of the public
/// `link_outlet_functions`.
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
/// `Err(SL_CLAIMED)` if another cell owns it. The check point of
/// `outlet_write_retained`.
pub(crate) fn gate<S: CellIdentity>(state: &S) -> Result<(), i32> {
    match state.sl_identity() {
        Some(id) if !claim_or_check(id) => Err(SL_CLAIMED),
        _ => Ok(()),
    }
}

/// The signal layer's claim state. One per node (this cell-host process
/// serves every cell on the node).
struct Claim {
    /// Current owner, or `None` when unclaimed.
    owner: Option<(Sri, Gen)>,
    /// Identities already warned about under the current owner, so a cell that
    /// retries every tick logs once, not forever.
    // ponytail: only emptied on takeover or release, so it grows with the
    // distinct refused identities under one long-lived owner; use a bounded
    // set if that ever matters.
    warned: Vec<(Sri, Gen)>,
}

static CLAIM: Mutex<Claim> = Mutex::new(Claim {
    owner: None,
    warned: Vec::new(),
});

/// Claim the SL for `who` if unclaimed, or confirm `who` already owns it.
/// A strictly newer `Gen` of the owner's `Sri` takes the claim over.
///
/// Returns `true` if `who` may write outlets, `false` if a different cell (or
/// an older incarnation of the owner) holds the claim. The first refusal of
/// each identity is logged.
pub(crate) fn claim_or_check(who: (Sri, Gen)) -> bool {
    let mut claim = CLAIM
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match claim.owner {
        None => {
            claim.owner = Some(who);
            true
        }
        Some(current) if current == who => true,
        Some(current) if current.0 == who.0 && who.1 > current.1 => {
            claim.owner = Some(who);
            claim.warned.clear();
            true
        }
        Some(current) => {
            if !claim.warned.contains(&who) {
                claim.warned.push(who);
                tracing::warn!(
                    owner_sri = %current.0,
                    owner_gen = %current.1,
                    caller_sri = %who.0,
                    caller_gen = %who.1,
                    "signal layer outlet write refused: claimed by another cell"
                );
            }

            false
        }
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
    let mut claim = CLAIM
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if claim.owner == Some(who) {
        claim.owner = None;
        claim.warned.clear();
    }
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    let mut claim = CLAIM
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    claim.owner = None;
    claim.warned.clear();
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

        // A strictly newer Gen of the same Sri takes over; the old one is refused.
        reset_for_test();
        let old = id(1, 10);
        let new = id(1, 11);
        assert!(claim_or_check(old));
        assert!(claim_or_check(new), "newer gen of same Sri takes over");
        assert!(
            !claim_or_check(old),
            "old incarnation refused after takeover"
        );

        release(old);
        assert!(
            claim_or_check(new),
            "old drop must not free the new owner's claim"
        );
        assert!(!claim_or_check(old), "old still refused after its release");

        assert!(
            !claim_or_check(id(2, 99)),
            "different Sri refused even with a newer gen"
        );
        assert!(
            !claim_or_check(id(1, 9)),
            "older gen of the owner's Sri does not take over"
        );

        // Each refused identity is recorded (warned) exactly once.
        assert!(!claim_or_check(old));
        let claim = CLAIM
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(claim.warned.iter().filter(|w| **w == old).count(), 1);
    }
}
