pub mod cell_lost;
pub mod class_registry;
pub(crate) mod commands;
pub(crate) mod deployment_error;
pub mod instance_registry;
pub(crate) mod lifecycle;
pub(crate) mod placement;
pub mod root_death;
pub mod root_restart;
pub mod spawn_gate;

pub const CMD_DEPLOY: &str = "deploy_cell";
