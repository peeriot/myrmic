//! Generation fencing for lifecycle rows.
//!
//! A placement or instance row is one incarnation's record. Every mutation
//! names the incarnation it acts for and is refused when the row belongs to
//! another one, so a late rollback or undeploy cannot take down a successor
//! that reused the SRI. The check and the write share one write transaction,
//! and the store's optimistic transactions fail a commit that raced another
//! writer of the same row, so the check cannot go stale in between.

use cell_protocol::Gen;

/// What a fenced row mutation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceOutcome {
    /// The row carried the expected generation and the mutation was applied.
    Applied,
    /// There was no row to act on.
    Absent,
    /// The row belongs to another incarnation; nothing was written.
    Superseded {
        /// The generation the row carries.
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

/// Whether a mutation issued for `expected` may touch a row of generation
/// `existing`; the `Err` is the outcome to report when it may not.
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
