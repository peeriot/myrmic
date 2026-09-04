use cell_protocol::{RuntimeId, Sri};
use myrmic_common::cells::{Command, Event};
use zenoh::key_expr::{KeyExpr, OwnedKeyExpr, format::keformat};

use crate::{DeploymentId, PortId, Result, TaskId, custom_err};

/* Topics served by the daemon */
/// Topic for all control messages
pub const TOPIC_CTL: &str = "@sorg/@ctl/**";
/// Topic to query information about the runtimes in the system
pub const TOPIC_EXEC_RUNTIMES: &str = "@sorg/@ctl/@runtimes/@exec";
pub const TOPIC_ORCH_RUNTIMES: &str = "@sorg/@ctl/@runtimes/@orch";

/* Topics served by the execution plugin */
const TOPIC_EXEC_INIT_PREFIX: &str = "@sorg/@execution/@init";
const TOPIC_EXEC_START_PREFIX: &str = "@sorg/@execution/@start";
const TOPIC_EXEC_DELETE_PREFIX: &str = "@sorg/@execution/@delete";
pub const TOPIC_EXEC_STATUS: &str = "@sorg/@execution/@status";

/* Topics served by the orchestation plugin */
pub const TOPIC_ORCH_DELETE: &str = "@sorg/@orchestration/@delete";
pub const TOPIC_ORCH_INIT: &str = "@sorg/@orchestration/@init";
pub const TOPIC_ORCH_START: &str = "@sorg/@orchestration/@start";
pub const TOPIC_ORCH_ERRORS: &str = "@sorg/@orchestration/@errors";
pub const TOPIC_ORCH_CELL_DEPLOY: &str = "@sorg/@orchestration/@cells/@deploy";
pub const TOPIC_ORCH_CELL_UNDEPLOY: &str = "@sorg/@orchestration/@cells/@undeploy";
/// Delete every cell sharing an app name (the `--app` / tree-delete path).
pub const TOPIC_ORCH_APP_DELETE: &str = "@sorg/@orchestration/@app/@delete";

/* Topics served by the execution plugin for cell lifecycle */
const TOPIC_EXEC_CELL_DEPLOY_PREFIX: &str = "@sorg/@execution/@cells/@deploy";
const TOPIC_EXEC_CELL_UNDEPLOY_PREFIX: &str = "@sorg/@execution/@cells/@undeploy";

/// Topic used to request a specific execution runtime to deploy a cell
#[must_use]
pub fn topic_execution_cell_deploy(rt_id: impl Into<RuntimeId>) -> String {
    let rt_id = rt_id.into();
    format!("{TOPIC_EXEC_CELL_DEPLOY_PREFIX}/{rt_id}")
}

/// Topic used to request a specific execution runtime to undeploy a cell
#[must_use]
pub fn topic_execution_cell_undeploy(rt_id: impl Into<RuntimeId>) -> String {
    let rt_id = rt_id.into();
    format!("{TOPIC_EXEC_CELL_UNDEPLOY_PREFIX}/{rt_id}")
}

/* Topics used internally */
const TOPIC_APPS_COMMUNICATION_PREFIX: &str = "@sorg/@applications/@communication";

/* Topics offered by sorg-external components */

/// Topic used to request a runtime to init a deployment
#[must_use]
pub fn topic_execution_init(rt_id: impl Into<RuntimeId>) -> String {
    let rt_id = rt_id.into();
    format!("{TOPIC_EXEC_INIT_PREFIX}/{rt_id}")
}

/// Topic used to request a runtime to start a deployment
#[must_use]
pub fn topic_execution_start(rt_id: impl Into<RuntimeId>) -> String {
    let rt_id = rt_id.into();
    format!("{TOPIC_EXEC_START_PREFIX}/{rt_id}")
}

/// Topic used to request a runtime to delete a deployment
#[must_use]
pub fn topic_execution_delete(rt_id: impl Into<RuntimeId>) -> String {
    let rt_id = rt_id.into();
    format!("{TOPIC_EXEC_DELETE_PREFIX}/{rt_id}")
}

/// Topic used to transmit messages between a Sender and a Receiver
/// - ``depl_id``: the ID of the deployment that the sender and receiver are part of
/// - ``task_id``: the ID of the task whose output is being transmitted between sender and receiver
/// - ``port_id``: the ID of the task output which is being transmitted between sender and receiver
///
/// # Panics
/// Would theoretically panic if provided with inputs which lead to the creation of an invalid topic. Hereby, the `TaskId` could be a problem since we build it from a String
#[must_use]
pub fn topic_app_communication(
    depl_id: &DeploymentId,
    task_id: &TaskId,
    port_id: &PortId,
) -> OwnedKeyExpr {
    format!(
        "{TOPIC_APPS_COMMUNICATION_PREFIX}/{depl_id}/{task_id}/{port_id}",
        task_id = task_id.as_ref(),
        port_id = port_id.as_ref()
    )
    .try_into()
    .expect("generating KE from the app comm topic should always work")
}

/* Topics related to cells */

zenoh::key_expr::format::kedefine!(
    // Used to send a command to a cell
    pub sorg_cell_command: "@sorg/@cells/command/${sri:*}/${cmd_name:*}",
    pub sorg_cell_queryable: "@sorg/@cells/queryable/${sri:*}/${queryable_name:*}",
    pub sorg_cell_event: "@sorg/@cells/event/${event_name:*}",
);

pub fn topic_commands_specific_cell(sri: Sri) -> Result<OwnedKeyExpr> {
    let topic = keformat!(sorg_cell_command::formatter(), sri = sri, cmd_name = "*")
        .map_err(|e| custom_err!("error formatting cell cmds topic: {e}"))?;
    Ok(topic)
}

pub fn cmd_name_from_ke(cmd_ke: &KeyExpr<'_>) -> Result<Command> {
    let parsed = sorg_cell_command::parse(cmd_ke)
        .map_err(|e| custom_err!("failed to parse command name from cmd ke: {e}"))?;
    let cmd_kd = parsed.cmd_name();
    let cmd_str = cmd_kd.to_string();
    let cmd: Command = cmd_str
        .try_into()
        .map_err(|e| custom_err!("failed to parse command name from cmd ke: {e}"))?;
    Ok(cmd)
}

pub fn topic_command_on_cell(sri: &Sri, cmd: &Command) -> Result<OwnedKeyExpr> {
    let topic = keformat!(
        sorg_cell_command::formatter(),
        sri = sri,
        cmd_name = cmd.as_ref()
    )
    .map_err(|e| custom_err!("error formatting cell cmd topic: {e}"))?;
    Ok(topic)
}

pub fn topic_cell_event(event: &Event) -> Result<OwnedKeyExpr> {
    let topic = keformat!(sorg_cell_event::formatter(), event_name = event.as_ref())
        .map_err(|e| custom_err!("error formatting cell event topic: {e}"))?;
    Ok(topic)
}

pub fn topic_queryables_specific_cell(sri: Sri) -> Result<OwnedKeyExpr> {
    let topic = keformat!(
        sorg_cell_queryable::formatter(),
        sri = sri,
        queryable_name = "*"
    )
    .map_err(|e| custom_err!("error formatting cell queryable topic: {e}"))?;
    Ok(topic)
}
