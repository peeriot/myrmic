use cell_protocol::Sri;
use myrmic_common::cells::Event;
use wasmtime::{Instance, Store};

use crate::wasm::cell::cell_task::{Outcome, log_crash};
use crate::wasm::cell::state::CellState;
use crate::wasm::cell::{CellEvent, sri_parts};

/// Runs an event's handler. A failed or trapped run rolls the event's
/// transaction back; a trapped one also ends the incarnation.
pub(super) async fn handle_event(
    instance: &Instance,
    store: &mut Store<CellState>,
    sri: &Sri,
    event: CellEvent,
) -> Outcome {
    let CellEvent {
        event,
        payload,
        sender,
    } = event;

    let fn_name = event_fn_name(&event);

    // The subscriber's own identity and the publisher's identity, split into
    // the `(hi, lo)` i64 pairs the guest recombines into `Metadata`.
    let (id_hi, id_lo) = sri_parts(Some(store.data().sri().as_uuid()));
    let (sender_hi, sender_lo) = sri_parts(sender);

    let n_bytes: i32 = payload
        .len()
        .try_into()
        .expect("event payload should be reasonably small");

    if let Err(err) = store.data_mut().store_arguments(payload) {
        tracing::error!("cell '{sri}' failed to store event arguments: {err}");
        return Outcome::Failed;
    }

    let params = (id_hi, id_lo, sender_hi, sender_lo, n_bytes);
    let exit_code =
        match instance.get_typed_func::<(i64, i64, i64, i64, i32), i32>(&mut (*store), &fn_name) {
            Ok(func) => match func.call_async(&mut (*store), params).await {
                Ok(code) => code,
                Err(trap) => {
                    log_crash(sri, "event", event.as_ref(), &trap);
                    return Outcome::Trapped;
                }
            },
            Err(err) => {
                // Nothing ran, so there is nothing to undo.
                tracing::warn!("cell '{sri}' event export '{fn_name}' not found: {err}");
                return Outcome::Succeeded;
            }
        };

    if exit_code != 0 {
        let stored_err_msg = store
            .data_mut()
            .take_err_msg()
            .unwrap_or("Module error; No err msg stored".to_owned());
        tracing::warn!(
            "cell '{sri}' error handling event '{e}': {stored_err_msg}",
            e = event.as_ref()
        );
        return Outcome::Failed;
    }

    Outcome::Succeeded
}

fn event_fn_name(event: &Event) -> String {
    format!("event_{event_name}", event_name = event.as_ref())
}
