use cell_protocol::Sri;
use tokio::sync::oneshot::Sender;
use wasmtime::{Instance, Store};

use crate::wasm::cell::CellTimerTick;
use crate::wasm::cell::cell_task::{Outcome, log_crash};
use crate::wasm::cell::state::CellState;

struct TimerCompletionToken(Option<Sender<()>>);

impl Drop for TimerCompletionToken {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

/// Runs a timer tick's handler. A failed or trapped run rolls the tick's
/// transaction back; a trapped one also ends the incarnation.
pub(super) async fn handle_timer_tick(
    instance: &Instance,
    store: &mut Store<CellState>,
    sri: &Sri,
    tick: CellTimerTick,
) -> Outcome {
    let CellTimerTick {
        timer_id,
        export_name,
        completed,
    } = tick;

    if !store.data().is_timer_active(timer_id) {
        tracing::trace!(
            "dropping stale tick for cancelled timer {timer_id} (export '{export_name}')"
        );
        return Outcome::Succeeded;
    }

    // A timer tick is the cell invoking itself on a schedule, so the sender is
    // the cell's own identity (id == sender).
    let (id_hi, id_lo) = sri.to_parts();
    let (sender_hi, sender_lo) = sri.to_parts();
    let n_bytes = 0;

    let params = (id_hi, id_lo, sender_hi, sender_lo, n_bytes);

    let _completed = TimerCompletionToken(completed);

    // The timer carries the bare command name (from the guest's `Callback`); the
    // exported symbol is `command_<name>`, matching `cmd_fn_name` on the command
    // path.
    let handler_export = format!("command_{export_name}");
    let func =
        instance.get_typed_func::<(i64, i64, i64, i64, i32), i32>(&mut (*store), &handler_export);

    let exit_code = match func {
        Ok(func) => match func.call_async(&mut (*store), params).await {
            Ok(code) => code,
            Err(trap) => {
                log_crash(sri, "timer", &export_name, &trap);
                return Outcome::Trapped;
            }
        },
        Err(err) => {
            // Nothing ran, so there is nothing to undo.
            tracing::warn!("cell '{sri}' timer export '{export_name}' not found: {err}");
            return Outcome::Succeeded;
        }
    };

    if exit_code != 0 {
        let err_msg = store
            .data_mut()
            .take_err_msg()
            .unwrap_or("Module error; No err msg stored".to_owned());
        tracing::warn!("cell '{sri}' error in timer tick '{export_name}': {err_msg}");
        return Outcome::Failed;
    }

    Outcome::Succeeded
}
