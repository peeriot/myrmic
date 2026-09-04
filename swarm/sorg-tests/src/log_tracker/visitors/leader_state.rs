use tracing::field::Visit;

use crate::{StateTracker, log_tracker::visitors::dbg_to_string};

/// Visitor that extracts leadership state changes from tracing events.
///
/// This visitor processes events containing leadership-related fields and extracts
/// information about node leadership changes.
///
/// # Examples
///
/// ## Event: Node becomes leader
/// ```rust
/// debug!(orch_lead = "elected", node_id = %node1_id, "Node became leader");
/// ```
/// **Result**: `leader_change = Some("elected")`, `node_id = Some("node1_id_string")`
///
/// ## Event: Node resigns leadership
/// ```rust
/// debug!(orch_lead = "resigned", node_id = %node2_id, "Node resigned");
/// ```
/// **Result**: `leader_change = Some("resigned")`, `node_id = Some("node2_id_string")`
///
/// In all other cases, the event is either completely ignored or does not contribute to changing the state
/// of the tracker
#[derive(Debug, Default)]
pub(crate) struct VisitorLeaderState {
    pub(crate) leader_change: Option<String>,
    pub(crate) node_id: Option<String>,
}

impl Visit for VisitorLeaderState {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "orch_lead" => {
                self.leader_change = Some(value.to_string());
            }
            "node_id" => {
                self.node_id = Some(value.to_string());
            }
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, zenoh_id: &dyn std::fmt::Debug) {
        if field.name() == "node_id" {
            self.node_id = Some(dbg_to_string(zenoh_id));
        }
    }
}

impl StateTracker {
    pub(crate) fn handle_leader_state_change(&self, visitor_leader: VisitorLeaderState) {
        if let (Some(change), Some(node_id)) =
            (visitor_leader.leader_change, visitor_leader.node_id)
        {
            let mut map = self.leader_state.lock().unwrap();
            match change.as_str() {
                "elected" => {
                    map.insert(node_id, true);
                }
                "resigned" => {
                    map.insert(node_id, false);
                }
                _ => {}
            }
        }
    }
}
