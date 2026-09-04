use db_commons::models::{
    NodeId, Version,
    locate::{PeerView, Response},
};

#[cfg(feature = "nano")]
use alloc::vec::Vec;

/// Flattens locate replies into routing candidates: each responder answered for
/// itself (so age zero), and vouched for the peers it named.
pub(crate) fn candidates(responses: impl IntoIterator<Item = Response>) -> Vec<PeerView> {
    responses
        .into_iter()
        .flat_map(|r| {
            core::iter::once(PeerView {
                id: r.id,
                age_ms: 0,
                head: r.head,
                state: r.state,
            })
            .chain(r.peers)
        })
        .collect()
}

/// Chooses which node to route a scoped transaction to, given every candidate
/// surfaced by a locate query — each responder's own entry plus the peers it
/// vouched for.
///
/// A candidate qualifies when it was seen within `max_age_ms` and (if
/// `min_version` is set) holds the scope at at least that version. Among those,
/// the most caught-up wins, and equal heads are broken by `scope`'s rendezvous
/// draw. `prefer_full` (writes) ranks non-draining holders first, before head —
/// see [`HolderState`](db_commons::models::locate::HolderState) for why the two
/// access kinds rank differently. `None` means nothing qualified, so the caller
/// must fall back.
///
/// Equal heads are not a corner case: `sys` and `sorg` are replicated by every
/// node unconditionally, so a placement read, an exec-registry read or a
/// node-lease write sees every node in the mesh at an identical head. Breaking
/// those ties by raw id sent all of it to one host — the same global bias
/// `any_node` had. The rendezvous draw spreads them while keeping what matters:
/// it is a pure function of already-shared state, so a writer and a later
/// reader still agree on the holder, and it is the draw custody collapse and
/// the `any_node` fallback already use. Raw id remains the final tie-break, so
/// the ordering is still total.
pub(crate) fn select_holder(
    candidates: impl IntoIterator<Item = PeerView>,
    scope: &db_commons::models::Scope,
    min_version: Option<Version>,
    max_age_ms: u64,
    prefer_full: bool,
) -> Option<NodeId> {
    use db_commons::models::locate::HolderState;
    use db_commons::models::rendezvous_hash;

    candidates
        .into_iter()
        .filter(|c| c.age_ms <= max_age_ms)
        .filter(|c| min_version.is_none_or(|min| c.head >= min))
        // A write never lands on a drainer — its holdings must freeze for the
        // drain to complete, and the drain rejects routed writes anyway. With
        // no replica in the set, the caller's fallback path takes over.
        .filter(|c| !(prefer_full && matches!(c.state, HolderState::Draining)))
        .map(|c| {
            let full = prefer_full && matches!(c.state, HolderState::Replica);
            (full, c.head, rendezvous_hash(scope, &c.id), c.id)
        })
        .max()
        .map(|(_, _, _, id)| id)
}

// The logic is feature-agnostic; exercising it under the std (`replica`) build
// covers the nano build, and keeps `vec!`/`Vec` out of the no_std test compile.
#[cfg(all(test, feature = "replica"))]
mod tests {
    use super::*;

    use db_commons::models::Scope;
    use db_commons::models::locate::HolderState;

    fn node(n: u8) -> NodeId {
        [n; 16]
    }

    fn scope() -> Scope {
        Scope::new("t", "t", "p")
    }

    /// A scope whose rendezvous draw ranks `node(1)` above `node(2)` — the
    /// opposite of the raw-id order, so a tie-break test can tell the two
    /// apart.
    fn scope_favouring_the_lower_id() -> Scope {
        Scope::new("t", "t", "a")
    }

    fn peer(id: u8, age_ms: u64, head: Version) -> PeerView {
        PeerView {
            id: node(id),
            age_ms,
            head,
            state: HolderState::Replica,
        }
    }

    fn drainer(id: u8, age_ms: u64, head: Version) -> PeerView {
        PeerView {
            state: HolderState::Draining,
            ..peer(id, age_ms, head)
        }
    }

    #[test]
    fn a_write_never_selects_a_draining_holder() {
        assert_eq!(
            select_holder(vec![drainer(1, 0, 10)], &scope(), None, 60_000, true),
            None,
            "a write falls through to the fallback rather than landing on a drain",
        );
        assert_eq!(
            select_holder(vec![drainer(1, 0, 10)], &scope(), None, 60_000, false),
            Some(node(1)),
            "a read still ranks the drainer — it may be the freshest holder",
        );
    }

    #[test]
    fn picks_the_most_caught_up_live_candidate() {
        let candidates = vec![peer(1, 0, 10), peer(2, 0, 20)];

        let chosen = select_holder(candidates, &scope(), None, 60_000, false);

        assert_eq!(chosen, Some(node(2)));
    }

    #[test]
    fn excludes_candidates_last_seen_beyond_max_age() {
        // Node 2 holds the higher head but was last seen long ago; the live
        // node 1 must win rather than routing to a likely-dead peer.
        let candidates = vec![peer(1, 1_000, 10), peer(2, 90_000, 20)];

        let chosen = select_holder(candidates, &scope(), None, 60_000, false);

        assert_eq!(chosen, Some(node(1)));
    }

    #[test]
    fn candidates_include_each_responder_and_its_vouched_peers() {
        let responses = vec![Response {
            id: node(1),
            head: 10,
            peers: vec![drainer(2, 500, 9)],
            state: HolderState::Draining,
        }];

        let cands = candidates(responses);

        assert_eq!(cands.len(), 2);
        let responder = cands.iter().find(|c| c.id == node(1)).expect("responder");
        assert_eq!(
            responder.age_ms, 0,
            "we just heard from the responder itself"
        );
        assert_eq!(responder.head, 10);
        assert!(
            matches!(responder.state, HolderState::Draining),
            "the responder's own state must survive flattening"
        );
        let vouched = cands
            .iter()
            .find(|c| c.id == node(2))
            .expect("vouched peer");
        assert_eq!(vouched.age_ms, 500);
        assert_eq!(vouched.head, 9);
        assert!(matches!(vouched.state, HolderState::Draining));
    }

    #[test]
    fn returns_none_when_no_candidate_meets_min_version() {
        // A min_version only a caught-up node could serve: neither candidate
        // holds the scope that far, so the caller must fall back rather than
        // route to a node that would reject the bound.
        let candidates = vec![peer(1, 0, 5), peer(2, 0, 20)];

        let chosen = select_holder(candidates, &scope(), Some(30), 60_000, false);

        assert_eq!(chosen, None);
    }

    #[test]
    fn writes_prefer_a_replica_over_a_more_caught_up_drainer() {
        // Right after a demotion the drainer is always ahead — it was the
        // write sink. Routing writes to the replica anyway is what freezes the
        // drainer's holdings so its coverage becomes a fixed post.
        let candidates = vec![drainer(1, 0, 20), peer(2, 0, 10)];

        let chosen = select_holder(candidates, &scope(), None, 60_000, true);

        assert_eq!(chosen, Some(node(2)));
    }

    #[test]
    fn writes_fall_back_when_only_a_drainer_answers() {
        // A drainer's holdings must freeze for its drain to complete, so a
        // write is never *routed* onto one — the any-node fallback takes over
        // (and may still physically land there, as a consolidatable stray).
        // A true replica outage heals through the drain's escalation instead.
        let candidates = vec![drainer(1, 0, 20)];

        let chosen = select_holder(candidates, &scope(), None, 60_000, true);

        assert_eq!(chosen, None);
    }

    #[test]
    fn reads_prefer_the_highest_head_even_on_a_drainer() {
        // Staleness cuts both ways: the drainer is the freshest (possibly
        // only) holder of its undrained rows, so reads rank by head alone.
        let candidates = vec![drainer(1, 0, 20), peer(2, 0, 10)];

        let chosen = select_holder(candidates, &scope(), None, 60_000, false);

        assert_eq!(chosen, Some(node(1)));
    }

    #[test]
    fn equal_heads_are_broken_by_the_scopes_draw_not_the_highest_id() {
        // Both hold the scope at the same version — the case every `sys`/`sorg`
        // read hits, since every node replicates those. Ranking by raw id would
        // send all of them to node 2 forever.
        let candidates = vec![peer(1, 0, 10), peer(2, 0, 10)];

        let chosen = select_holder(
            candidates,
            &scope_favouring_the_lower_id(),
            None,
            60_000,
            false,
        );

        assert_eq!(
            chosen,
            Some(node(1)),
            "the draw decides the tie, so different scopes land on different nodes",
        );
    }

    #[test]
    fn the_draw_never_outranks_a_more_caught_up_holder() {
        // Freshness first: the draw only orders candidates that are level.
        let candidates = vec![peer(1, 0, 10), peer(2, 0, 20)];

        let chosen = select_holder(
            candidates,
            &scope_favouring_the_lower_id(),
            None,
            60_000,
            false,
        );

        assert_eq!(chosen, Some(node(2)));
    }
}
