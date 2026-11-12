//! Whether a swarm is connected as expected, judged from the status every node reports for itself.

use std::collections::{BTreeMap, BTreeSet};

use introspection_common::v1::NodeStatus;
use zenoh::config::ZenohId;

/// Why `statuses` do not show the swarm of `expected`, or `None` if they do: exactly the runtimes
/// of `expected` answer, and each of them is linked to exactly the runtimes `expected` lists for
/// it.
///
/// The runtimes are the nodes that report no `participant`. CLI sessions report one, and the
/// session that asks does not answer at all, so neither counts, also not among a runtime's links.
/// A runtime's links are its peers and its routers alike: a node in zenoh's router mode is listed
/// under routers, but is linked all the same.
pub(super) fn mismatch(
    statuses: &[NodeStatus],
    expected: &BTreeMap<ZenohId, BTreeSet<ZenohId>>,
) -> Option<String> {
    let runtime_statuses = || {
        statuses
            .iter()
            .filter(|status| status.participant.is_none())
    };
    let runtimes: BTreeSet<ZenohId> = runtime_statuses().map(|status| status.id).collect();
    let expected_runtimes: BTreeSet<ZenohId> = expected.keys().copied().collect();
    if runtimes == expected_runtimes {
        runtime_statuses().find_map(|status| {
            let links: BTreeSet<ZenohId> = status
                .peers
                .iter()
                .chain(&status.routers)
                .copied()
                .filter(|link| runtimes.contains(link))
                .collect();
            let expected_links = &expected[&status.id];
            (links != *expected_links).then(|| {
                format!(
                    "runtime {} is linked to the runtimes {links:?}, expected {expected_links:?}",
                    status.id
                )
            })
        })
    } else {
        Some(format!(
            "the runtimes {runtimes:?} answered, expected {expected_runtimes:?}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use introspection_common::v1::{NodeStatus, ParticipantInfo};
    use zenoh::config::ZenohId;

    use super::mismatch;

    fn zid(id: &str) -> ZenohId {
        id.parse().expect("test zids are valid hex")
    }

    fn zids(ids: &[&str]) -> Vec<ZenohId> {
        ids.iter().map(|id| zid(id)).collect()
    }

    fn runtime(id: &str, peers: &[&str], routers: &[&str]) -> NodeStatus {
        NodeStatus {
            id: zid(id),
            participant: None,
            peers: zids(peers),
            routers: zids(routers),
            plugins: vec![],
        }
    }

    /// each runtime with the runtimes it is expected to be linked to
    fn expected(links: &[(&str, &[&str])]) -> BTreeMap<ZenohId, BTreeSet<ZenohId>> {
        links
            .iter()
            .map(|(id, linked)| (zid(id), zids(linked).into_iter().collect()))
            .collect()
    }

    #[test]
    fn a_full_mesh_matches() {
        let statuses = [
            runtime("a1", &["a2", "a3"], &[]),
            runtime("a2", &["a1", "a3"], &[]),
            runtime("a3", &["a1", "a2"], &[]),
        ];
        let expected = expected(&[
            ("a1", &["a2", "a3"]),
            ("a2", &["a1", "a3"]),
            ("a3", &["a1", "a2"]),
        ]);
        assert_eq!(mismatch(&statuses, &expected), None);
    }

    #[test]
    fn a_router_counts_as_a_link() {
        // `a3` runs in router mode: the others list it under routers
        let statuses = [
            runtime("a1", &["a2"], &["a3"]),
            runtime("a2", &["a1"], &["a3"]),
            runtime("a3", &["a1", "a2"], &[]),
        ];
        let expected = expected(&[
            ("a1", &["a2", "a3"]),
            ("a2", &["a1", "a3"]),
            ("a3", &["a1", "a2"]),
        ]);
        assert_eq!(mismatch(&statuses, &expected), None);
    }

    #[test]
    fn cli_sessions_and_the_asking_session_do_not_count() {
        // `c1` is a CLI that answers with a participant, `f1` the session that asks
        let cli = NodeStatus {
            participant: Some(ParticipantInfo {
                kind: "cli".to_owned(),
                name: "myrmic cells status".to_owned(),
                origin: None,
            }),
            ..runtime("c1", &["a1"], &[])
        };
        let statuses = [
            runtime("a1", &["a2", "c1", "f1"], &[]),
            runtime("a2", &["a1", "f1"], &[]),
            cli,
        ];
        let expected = expected(&[("a1", &["a2"]), ("a2", &["a1"])]);
        assert_eq!(mismatch(&statuses, &expected), None);
    }

    #[test]
    fn a_missing_runtime_is_a_mismatch() {
        let statuses = [runtime("a1", &[], &[])];
        let expected = expected(&[("a1", &["a2"]), ("a2", &["a1"])]);
        assert!(mismatch(&statuses, &expected).is_some());
    }

    #[test]
    fn a_foreign_runtime_is_a_mismatch() {
        let statuses = [
            runtime("a1", &["a2", "b1"], &[]),
            runtime("a2", &["a1", "b1"], &[]),
            runtime("b1", &["a1", "a2"], &[]),
        ];
        let expected = expected(&[("a1", &["a2"]), ("a2", &["a1"])]);
        assert!(mismatch(&statuses, &expected).is_some());
    }

    #[test]
    fn a_missing_link_is_a_mismatch() {
        let statuses = [
            runtime("a1", &["a2"], &[]),
            runtime("a2", &["a1", "a3"], &[]),
            runtime("a3", &["a2"], &[]),
        ];
        let expected = expected(&[
            ("a1", &["a2", "a3"]),
            ("a2", &["a1", "a3"]),
            ("a3", &["a1", "a2"]),
        ]);
        assert!(mismatch(&statuses, &expected).is_some());
    }

    #[test]
    fn an_unexpected_link_is_a_mismatch() {
        // `a1` and `a3` are on different groups, but linked anyway
        let statuses = [
            runtime("a1", &["a2", "a3"], &[]),
            runtime("a2", &["a1", "a3"], &[]),
            runtime("a3", &["a1", "a2"], &[]),
        ];
        let expected = expected(&[("a1", &["a2"]), ("a2", &["a1", "a3"]), ("a3", &["a2"])]);
        assert!(mismatch(&statuses, &expected).is_some());
    }
}
