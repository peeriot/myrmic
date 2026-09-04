use cell_protocol::RuntimeId;
use claims::{assert_matches, assert_some};
use sorg_common::{DeploymentId, TaskId};
use std::{str::FromStr, time::Duration};
use tracing::{debug, trace};
use zenoh::config::ZenohId;

use crate::{log_tracker::TaskStatus, set_up_log_tracker};

#[test]
fn log_tracker_tests() {
    // Running sequentially since the trackers are reset
    log_tracker_captures_leader_events();
    log_tracker_captures_task_events();
    log_tracker_captures_wasm_output();
}

fn log_tracker_captures_wasm_output() {
    let tracker = set_up_log_tracker();
    let module_id = "my module".to_owned();
    let message = "hello".to_owned();

    trace!(
        wasm_output = %message,
        module_id = %module_id
    );

    assert!(tracker.get_modules_with_output().contains(&module_id));
    let output = assert_some!(tracker.get_output_of_module(&module_id));
    assert!(output.contains(&message));
}

fn log_tracker_captures_leader_events() {
    let tracker = set_up_log_tracker();
    let node1_id = ZenohId::from_str("37c72f467bc9c77f41b73fe16f054741").unwrap();

    debug!(
        orch_lead = "elected",
        node_id = %node1_id,
        "Node became leader",
    );

    assert!(tracker.check_is_leader(&node1_id.to_string()));
    assert_eq!(tracker.get_current_leader(), Some(node1_id.to_string()));
    assert_eq!(
        tracker.get_leader_info(),
        [(node1_id.to_string(), true)].iter().cloned().collect()
    );

    debug!(orch_lead = "resigned", node_id = %node1_id, "Node resigned leadership");

    assert!(!tracker.check_is_leader(&node1_id.to_string()));
    assert_eq!(tracker.get_current_leader(), None);
}

fn log_tracker_captures_task_events() {
    let tracker = set_up_log_tracker();
    let node1_id = ZenohId::from_str("37c72f467bc9c77f41b73fe16f054741").unwrap();

    let rt_id = RuntimeId::from(node1_id);
    let depl_id = DeploymentId::default();
    let task_id = TaskId::try_from("task_one".to_owned()).unwrap();

    trace!(
        deployment = "init",
        runtime_id = %rt_id,
        depl_id = %depl_id,
        task_id = %task_id,
        node_id = %node1_id,
        "task init"
    );

    std::thread::sleep(Duration::from_millis(500));
    assert_matches!(
        tracker.check_task_status(&node1_id.to_string(), &depl_id, &task_id),
        Some(TaskStatus::Init)
    );
    assert_eq!(
        tracker.check_task_rt(&node1_id.to_string(), &depl_id, &task_id),
        Some(rt_id)
    );

    trace!(
        deployment = "start",
        runtime_id = %rt_id,
        depl_id = %depl_id,
        task_id = %task_id,
        node_id = %node1_id,
            "task start"
    );

    assert_matches!(
        tracker.check_task_status(&node1_id.to_string(), &depl_id, &task_id),
        Some(TaskStatus::Running)
    );

    trace!(
        deployment = "delete",
        runtime_id = %rt_id,
        depl_id = %depl_id,
        task_id = %task_id,
        node_id = %node1_id,
        "task delete"
    );

    assert_eq!(tracker.check_task_num(), 0);
}
