//! Module for the infrastructure used to track the emitted logs and figure out the state of the system from them.
//! On this top-level, there are methods for setup and the general functionality. See the api module for the methods
//! providing information. See the visitor module to see which logs provide which information.

use std::sync::{Arc, Mutex, OnceLock};
use tracing::{Event, Subscriber};
use tracing_subscriber::{EnvFilter, Layer, layer::Context, prelude::*, registry::LookupSpan};

use crate::log_tracker::visitors::{VisitorLeaderState, VisitorOutput, VisitorTaskState};

mod api;
mod visitors;

pub(crate) use api::TaskInfo;
pub use api::{StateTracker, TaskStatus};

#[cfg(test)]
mod tests;

// Global singleton instance
static TRACKER_MANAGER: OnceLock<Arc<Mutex<StateTracker>>> = OnceLock::new();

/// Set up the `StateTracker` as a tracing layer for testing
///
/// This function ensures that:
/// - The tracing layer is only registered once across all tests
/// - Each call returns a tracker with fresh, reset state
/// - No duplicate layers are created
#[must_use]
pub fn set_up_log_tracker() -> StateTracker {
    let tracker = TRACKER_MANAGER
        .get_or_init(|| {
            let tracker = Arc::new(Mutex::new(StateTracker::new()));

            // Register the tracing layer (this only happens once)
            // Extract StateTracker for registration since Layer is implemented for StateTracker, not Arc<Mutex<StateTracker>>
            let state_tracker = tracker.lock().unwrap().clone();
            let _ = tracing_subscriber::registry()
                .with(EnvFilter::new(
                    "sorg_orchestration=trace,sorg_tests=trace,sorg_execution=trace,zenoh_plugin_db=trace,warn",
                ))
                .with(tracing_subscriber::fmt::layer().with_test_writer())
                .with(state_tracker.clone())
                .try_init();

            tracker
        })
        .clone();

    // Always reset state before returning
    let mut state_tracker = tracker.lock().unwrap();
    state_tracker.reset_state();
    state_tracker.clone()
}

impl<S> Layer<S> for StateTracker
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    // Called for each tracing event that happens
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor_leadership = VisitorLeaderState::default();
        let mut visitor_task_state = VisitorTaskState::default();
        let mut visitor_output = VisitorOutput::default();

        event.record(&mut visitor_leadership);
        self.handle_leader_state_change(visitor_leadership);

        event.record(&mut visitor_task_state);
        self.handle_task_state_change(visitor_task_state);

        event.record(&mut visitor_output);
        self.handle_wasm_output(visitor_output);
    }
}
