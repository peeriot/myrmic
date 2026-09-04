use tracing::field::Visit;

use crate::{StateTracker, log_tracker::visitors::dbg_to_string};

/// Visitor that extracts messages originating from within Wasm modules from tracing events emitted by the Wasm host.
///
/// This visitor processes events containing output-related fields and extracts the messages issued there.
///
/// # Examples
///
/// ## Event: Hello world message from within Wasm
/// ```rust
/// let msg = "hello world from Wasm!";
/// let module_id = "bla";
/// trace!(wasm_output = %msg, module_id = %module_id, "Wasm module logged output");
/// ```
/// **Result**: `message = Some("hello world from Wasm!")`, `module_id = Some("bla")`
///
/// In all other cases, the event is either completely ignored or does not contribute to changing the state
/// of the tracker
#[derive(Debug, Default)]
pub(crate) struct VisitorOutput {
    pub(crate) message: Option<String>,
    pub(crate) module_id: Option<String>,
}

impl Visit for VisitorOutput {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "wasm_output" => self.message = Some(dbg_to_string(value)),
            "module_id" => self.module_id = Some(dbg_to_string(value)),
            _ => {}
        }
    }
}

impl StateTracker {
    pub(crate) fn handle_wasm_output(&self, visitor_output: VisitorOutput) {
        if let (Some(message), Some(module_id)) = (visitor_output.message, visitor_output.module_id)
        {
            let mut map = self.module_output.lock().unwrap();
            map.entry(module_id).or_default().push(message);
        }
    }
}
