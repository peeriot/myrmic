//! Generation fencing for lifecycle rows.
//!
//! Placement and instance rows are kept per generation, one row per
//! incarnation of a cell (see [`cell_protocol::generations`]); the newest is
//! the cell's current one. Every mutation names the incarnation it acts for. A
//! write is refused unless that incarnation is still the newest, so a late
//! deploy cannot publish itself over a successor that reused the SRI. A
//! removal only ever touches its own incarnation's row, so it can't take down
//! a successor and is never refused. The check and the write share one write
//! transaction, and the store's optimistic transactions fail a commit that
//! raced another writer of the same row, so the check cannot go stale in
//! between.

use cell_protocol::Gen;

/// What a fenced row mutation found, and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceOutcome {
    /// The mutation's incarnation was the cell's newest, and the mutation was
    /// applied.
    Applied,
    /// The cell had no row at all; nothing was written.
    Absent,
    /// The cell's newest incarnation is another one. A write wrote nothing; a
    /// removal still removed its own incarnation's row, if it was there.
    Superseded {
        /// The cell's newest generation.
        current: Gen,
    },
}

impl FenceOutcome {
    /// Whether the mutation went through.
    #[must_use]
    pub fn applied(self) -> bool {
        matches!(self, Self::Applied)
    }
}

/// Whether a write issued for incarnation `expected` may go ahead, given the
/// cell's newest generation `existing`; the `Err` is the outcome to report when
/// it may not.
pub(crate) fn admit(existing: Option<Gen>, expected: Gen) -> Result<(), FenceOutcome> {
    match existing {
        None => Err(FenceOutcome::Absent),
        Some(current) if current != expected => Err(FenceOutcome::Superseded { current }),
        Some(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn g(time: u64) -> Gen {
        Gen::from_parts(time, 1)
    }

    #[test]
    fn matching_generation_is_admitted() {
        assert_eq!(admit(Some(g(2)), g(2)), Ok(()));
    }

    #[test]
    fn missing_row_is_absent() {
        assert_eq!(admit(None, g(2)), Err(FenceOutcome::Absent));
    }

    #[test]
    fn any_other_generation_is_superseded() {
        // Older as well as newer: a fenced mutation only ever acts on its own row.
        assert_eq!(
            admit(Some(g(1)), g(2)),
            Err(FenceOutcome::Superseded { current: g(1) })
        );
        assert_eq!(
            admit(Some(g(3)), g(2)),
            Err(FenceOutcome::Superseded { current: g(3) })
        );
    }
}
