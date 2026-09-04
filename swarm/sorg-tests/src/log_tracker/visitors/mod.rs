//! Module for the different visitors; Each visitor records a type of event from the tracing logs

mod leader_state;
mod task_state;
mod wasm_output;

pub(crate) use leader_state::VisitorLeaderState;
pub(crate) use task_state::VisitorTaskState;
pub(crate) use wasm_output::VisitorOutput;

fn dbg_to_string(value: &dyn std::fmt::Debug) -> String {
    format!("{:?}", value).trim_matches('"').to_string()
}
