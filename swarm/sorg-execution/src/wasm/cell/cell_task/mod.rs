//! The per-cell runtime: owns a cell's Wasm instance and store and drives its
//! single message loop.

use std::time::Instant;

use cell_mailbox::Settlement;
use cell_protocol::Sri;
use sorg_common::{PoisonRcv, bail};
use tokio::sync::mpsc;
use tracing::debug;
use wasmtime::{Instance, Store};

use crate::{
    Result,
    wasm::cell::{
        CellCommand, CellMessage, CommandOrigin, IncomingMessage,
        cell_task::{commands::handle_command, events::handle_event, timers::handle_timer_tick},
        observability,
        state::CellState,
    },
};

pub(crate) mod commands;
pub(crate) mod events;
pub(crate) mod metrics;
mod timers;

/// Owns everything needed to run one cell: its Wasm instance and store, and the
/// channel that command/event/timer sources feed into. [`run`](Self::run) is
/// the cell's single message loop — the one place inbound messages are
/// dispatched (replacing the separate real-cell and bridge loops that used to
/// exist).
pub(crate) struct CellRuntime {
    instance: Instance,
    store: Store<CellState>,
    msg_rcv: mpsc::Receiver<IncomingMessage>,
    sri: Sri,
    runner_fuel: u64,
}

impl CellRuntime {
    pub(crate) fn new(
        instance: Instance,
        store: Store<CellState>,
        msg_rcv: mpsc::Receiver<IncomingMessage>,
        sri: Sri,
        runner_fuel: u64,
    ) -> Self {
        Self {
            instance,
            store,
            msg_rcv,
            sri,
            runner_fuel,
        }
    }

    /// Runs the message loop until the cell is poisoned (actively terminated)
    /// or its message channel closes.
    pub(crate) async fn run(self, poison_rcv: PoisonRcv) -> Result<()> {
        let Self {
            instance,
            mut store,
            mut msg_rcv,
            sri,
            runner_fuel,
        } = self;

        tokio::select! {
            cell_result = Self::message_loop(&instance, &mut store, &mut msg_rcv, &sri, runner_fuel) => {
                cell_result
            }
            poison_result = poison_rcv => {
                match poison_result {
                    Ok(()) => debug!("cell actively terminated"),
                    Err(err) => debug!("cell terminated during cleanup: {err}"),
                }
                Ok(())
            }
        }
    }

    async fn message_loop(
        instance: &Instance,
        store: &mut Store<CellState>,
        msg_rcv: &mut mpsc::Receiver<IncomingMessage>,
        sri: &Sri,
        runner_fuel: u64,
    ) -> Result<()> {
        loop {
            let Some(incoming) = msg_rcv.recv().await else {
                bail!("message channel closed for cell {sri}");
            };

            // Fresh fuel per turn: the budget caps a single handler run, not the
            // cell's cumulative lifetime.
            store.set_fuel(runner_fuel)?;

            let recv_lag = incoming.queued_at.map(|queued_at| queued_at.elapsed());
            let began = Instant::now();
            let mut message = store.data_mut().begin_message(incoming);
            let span_open = began.elapsed();

            let ready = match &mut message {
                CellMessage::Command(cmd) => cmd.ready.take(),
                _ => None,
            };

            // Captured before the receipt goes into the handler's transaction:
            // whether that transaction commits is only known afterwards, and
            // until the stream is told it keeps suppressing redelivery.
            let settle = match &message {
                CellMessage::Command(CellCommand {
                    origin: CommandOrigin::Mailbox(receipt),
                    ..
                }) => Some(receipt.settle_handle()),
                _ => None,
            };

            // Captured before `message` is moved into the handler below, so it survives to
            // record the `*_processed` metric once the message has actually finished (as
            // opposed to `*_queued`, recorded when it first arrived — see `cell_task::metrics`).
            let processed = match &message {
                CellMessage::Command(cmd) => Some(Processed::Command(cmd.cmd.as_ref().to_owned())),
                CellMessage::Event(event) => {
                    Some(Processed::Event(event.event.as_ref().to_owned()))
                }
                CellMessage::TimerTick(_) | CellMessage::TimerFinished(_) => None,
            };

            let dispatched = Instant::now();
            let outcome = dispatch(instance, store, sri, message).await;
            metrics::record_turn_split(sri, recv_lag, dispatched.elapsed());

            // Always finish — the span and per-message state are cleaned up
            // whichever way the call went; only the transaction's fate differs.
            let finished = finish_message(store, outcome).await;
            metrics::record_span(sri, span_open + finished.span_close);

            if let Some(settle) = settle {
                settle.settle(finished.settlement);
            }

            // A failed call's transaction is rolled back and the mailbox message it
            // came from stays put for retry (at-least-once) — recording it here
            // regardless of `outcome` counted every retry attempt as its own
            // "processed" command/event, inflating the total past what was ever
            // actually injected. Only a committed outcome is one.
            // A retried command is still counted, just separately: keeping it out
            // of `processed` is what lets `loss` mean anything, and the retry
            // volume is worth a number of its own rather than vanishing.
            match (processed, outcome) {
                (Some(Processed::Command(name)), Outcome::Succeeded) => {
                    metrics::record_command_processed(sri, &name);
                }
                (Some(Processed::Command(name)), Outcome::Failed) => {
                    metrics::record_command_failed(sri, &name);
                }
                (Some(Processed::Event(name)), Outcome::Succeeded) => {
                    metrics::record_event_processed(sri, &name);
                }
                (Some(Processed::Event(_)), Outcome::Failed) | (None, _) => {}
            }

            if let Some(ready) = ready {
                let _ = ready.send(());
            }
        }
    }
}

/// Whether a cell function ran to completion. A failed call's transaction is
/// rolled back, so nothing it did — writes, emitted messages, the mailbox
/// delete that consumed it — survives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Succeeded,
    Failed,
}

impl Outcome {
    fn of(succeeded: bool) -> Self {
        if succeeded {
            Self::Succeeded
        } else {
            Self::Failed
        }
    }
}

async fn dispatch(
    instance: &Instance,
    store: &mut Store<CellState>,
    sri: &Sri,
    message: CellMessage,
) -> Outcome {
    match message {
        CellMessage::Command(cmd) => Outcome::of(handle_command(instance, store, sri, cmd).await),
        CellMessage::Event(event) => Outcome::of(handle_event(instance, store, sri, event).await),
        CellMessage::TimerTick(tick) => {
            Outcome::of(handle_timer_tick(instance, store, sri, tick).await)
        }
        CellMessage::TimerFinished(tick) => {
            store.data_mut().remove_finished_timer(tick.timer_id);
            Outcome::Succeeded
        }
    }
}

/// A message kind pending the `*_processed` metric, captured before the message is moved into
/// its handler (see [`CellRuntime::message_loop`]).
enum Processed {
    Command(String),
    Event(String),
}

/// What finishing a message cost and what became of it.
struct Finished {
    /// Closing the observability span, paired by the caller with opening it
    /// (see [`metrics::record_span`]).
    span_close: std::time::Duration,
    /// Reported to the mailbox, which cannot work it out by reading — the
    /// mailbox delete rides the handler's transaction, so only its commit says
    /// whether the command is gone.
    settlement: Settlement,
}

/// Finalizes a message: closes its observability span, clears per-message
/// state, and closes the transaction the call opened, if any — committed when
/// the call succeeded, rolled back when it did not.
async fn finish_message(store: &mut Store<CellState>, outcome: Outcome) -> Finished {
    let sri = *store.data().sri();
    let closing = Instant::now();
    let span = store.data_mut().take_current_span();
    observability::finish_observability(span);
    let span_close = closing.elapsed();

    store.data_mut().clear_arguments();

    // No transaction means nothing was deferred into one — including the
    // mailbox delete, so the command is still there.
    let Some(application) = store.data_mut().take_application() else {
        return Finished {
            span_close,
            settlement: Settlement::Retry,
        };
    };

    if outcome == Outcome::Failed {
        if let Err(err) = application.rollback().await {
            tracing::error!("failed to roll back handler transaction: {err}");
        }
        return Finished {
            span_close,
            settlement: Settlement::Retry,
        };
    }

    // The one round trip a command's service time is made of: the handler
    // itself runs in microseconds, so whatever a cell's throughput ceiling is,
    // it is this. Recorded as a sum and a count — a mean is enough to tell
    // "about a millisecond" from "about seven".
    let started = Instant::now();
    let committed = application.commit().await;
    metrics::record_commit(&sri, started.elapsed());

    let settlement = match committed {
        Ok(()) => Settlement::Consumed,
        Err(err) => {
            tracing::error!("failed to commit handler transaction: {err}");
            Settlement::Retry
        }
    };

    Finished {
        span_close,
        settlement,
    }
}
