//! Whether a swarm is connected as expected, judged from the status every node reports for itself.

use std::collections::BTreeSet;

use introspection_common::v1::NodeStatus;
use zenoh::config::ZenohId;

/// Why `statuses` do not show the swarm of `expected`, or `None` if they do: exactly the runtimes
/// of `expected` answer, and each of them is linked to all the others.
///
/// The runtimes are the nodes that report no `participant`. CLI sessions report one, and the
/// session that asks does not answer at all, so neither counts, also not among a runtime's peers.
pub(super) fn mismatch(statuses: &[NodeStatus], expected: &BTreeSet<ZenohId>) -> Option<String> {
    let runtime_statuses = || {
        statuses
            .iter()
            .filter(|status| status.participant.is_none())
    };
    let runtimes: BTreeSet<ZenohId> = runtime_statuses().map(|status| status.id).collect();
    if runtimes == *expected {
        runtime_statuses().find_map(|status| {
            let peers: BTreeSet<ZenohId> = status
                .peers
                .iter()
                .copied()
                .filter(|peer| runtimes.contains(peer))
                .collect();
            let others: BTreeSet<ZenohId> = expected
                .iter()
                .copied()
                .filter(|zid| *zid != status.id)
                .collect();
            (peers != others).then(|| {
                format!(
                    "runtime {} is linked to the runtimes {peers:?}, expected {others:?}",
                    status.id
                )
            })
        })
    } else {
        Some(format!(
            "the runtimes {runtimes:?} answered, expected {expected:?}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use introspection_common::v1::{NodeStatus, ParticipantInfo};
    use zenoh::config::ZenohId;

    use super::mismatch;

    fn zid(id: &str) -> ZenohId {
        id.parse().expect("test zids are valid hex")
    }

    fn runtime(id: &str, peers: &[&str]) -> NodeStatus {
        NodeStatus {
            id: zid(id),
            participant: None,
            peers: peers.iter().map(|peer| zid(peer)).collect(),
            routers: vec![],
            plugins: vec![],
        }
    }

    fn expected(ids: &[&str]) -> BTreeSet<ZenohId> {
        ids.iter().map(|id| zid(id)).collect()
    }

    #[test]
    fn a_full_mesh_matches() {
        let statuses = [
            runtime("a1", &["a2", "a3"]),
            runtime("a2", &["a1", "a3"]),
            runtime("a3", &["a1", "a2"]),
        ];
        assert_eq!(mismatch(&statuses, &expected(&["a1", "a2", "a3"])), None);
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
            ..runtime("c1", &["a1"])
        };
        let statuses = [
            runtime("a1", &["a2", "c1", "f1"]),
            runtime("a2", &["a1", "f1"]),
            cli,
        ];
        assert_eq!(mismatch(&statuses, &expected(&["a1", "a2"])), None);
    }

    #[test]
    fn a_missing_runtime_is_a_mismatch() {
        let statuses = [runtime("a1", &[])];
        assert!(mismatch(&statuses, &expected(&["a1", "a2"])).is_some());
    }

    #[test]
    fn a_foreign_runtime_is_a_mismatch() {
        let statuses = [
            runtime("a1", &["a2", "b1"]),
            runtime("a2", &["a1", "b1"]),
            runtime("b1", &["a1", "a2"]),
        ];
        assert!(mismatch(&statuses, &expected(&["a1", "a2"])).is_some());
    }

    #[test]
    fn a_missing_link_is_a_mismatch() {
        let statuses = [
            runtime("a1", &["a2"]),
            runtime("a2", &["a1", "a3"]),
            runtime("a3", &["a2"]),
        ];
        assert!(mismatch(&statuses, &expected(&["a1", "a2", "a3"])).is_some());
    }
}
