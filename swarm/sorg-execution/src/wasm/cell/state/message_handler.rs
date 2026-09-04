//! Manages inbound message delivery for a cell.
//!
//! Listens on Zenoh for commands (via queryables) and events (via
//! subscribers), translates the raw Zenoh payloads and attachments into
//! [`IncomingMessage`]s, and feeds them through a single mpsc channel to
//! the cell task. The [`IncomingMessage`] carries both the cell-level
//! payload ([`CellMessage`]) and transport-level metadata (e.g., span
//! context from attachments) that the middleware processes before dispatch.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use cell_protocol::{Gen, MailboxEvent, Sri};
use myrmic_common::cells::{CreateTimerRequest, Event};
use opentelemetry::KeyValue;
use sorg_common::{bail, custom_err};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{AbortHandle, JoinHandle};
use zenoh::Session;

use crate::Result;
use crate::wasm::cell::state::DropHandle;
#[cfg(feature = "ble-linux")]
use crate::wasm::cell::{CellCommand, CommandOrigin};
use crate::wasm::cell::{
    CellEvent, CellMessage, CellTimerFinished, CellTimerTick, IncomingMessage,
};

/// A clonable handle the BLE backend uses to deliver a result to the cell: it
/// enqueues the `command_<export_name>` handler the cell named as its callback,
/// with `payload` as the argument.
#[cfg(feature = "ble-linux")]
#[derive(Clone)]
pub(crate) struct BleCallbackSink {
    snd: mpsc::Sender<IncomingMessage>,
    sri: Sri,
}

#[cfg(feature = "ble-linux")]
impl BleCallbackSink {
    /// Delivers a callback, awaiting channel capacity. Drops it if the cell's
    /// message channel has closed (the cell is gone).
    pub(crate) async fn deliver(&self, export_name: String, payload: Vec<u8>) {
        let cmd = match myrmic_common::cells::Command::new(export_name) {
            Ok(cmd) => cmd,
            Err(err) => {
                tracing::error!(
                    "cell {sri} named an unusable ble callback: {err}",
                    sri = self.sri
                );
                return;
            }
        };

        // The cell is acting on its own registration, so it is its own sender.
        let message = CellMessage::Command(CellCommand {
            cmd,
            payload: Some(payload),
            origin: CommandOrigin::Local,
            ready: None,
            sender: Some(self.sri.as_uuid()),
        });

        let _ = self
            .snd
            .send(IncomingMessage {
                span_context: None,
                message,
                queued_at: None,
            })
            .await;
    }
}

const QUEUE_CAPACITY: usize = 10; // TODO: we need to specify what a reasonable number is and - more importantly - what we want to happen once the queue is full

/// Manages command queryable and dynamic event subscriptions for a cell, providing all messages through one channel served by the cell task
pub(crate) struct CellMessageHandler {
    session: Session,
    sri: Sri,
    /// Cell incarnation, paired with `sri` to release the signal-layer claim
    /// (swarm#1340) — a later incarnation is a distinct owner.
    gen_id: Gen,

    message_snd: mpsc::Sender<IncomingMessage>,
    command_task: DropHandle,
    event_tasks: HashMap<String, JoinHandle<()>>,
    event_handlers: HashSet<String>,
    timer_tasks: HashMap<u32, AbortHandle>,
    next_timer_id: u32,
    mailbox_poll_interval: Duration,
    mailbox_batch_size: usize,
}

impl CellMessageHandler {
    pub(crate) fn new(
        session: Session,
        sri: Sri,
        gen_id: Gen,
        event_handlers: HashSet<String>,
        mailbox_poll_interval: Duration,
        mailbox_batch_size: usize,
    ) -> (Self, mpsc::Receiver<IncomingMessage>, oneshot::Receiver<()>) {
        let (message_tx, message_rx) = mpsc::channel(QUEUE_CAPACITY);

        let db = db_client::v1::Client::new(&session);

        // TODO: If we want to optimize, we could check whether a cell serves any commands / queries at all and only spin up the tasks if it does
        let (command_task, ready_rcv) = super::commands::spawn_command_producer(
            &db,
            sri,
            message_tx.clone(),
            mailbox_poll_interval,
            mailbox_batch_size,
        );

        let msg_handler = Self {
            session,
            sri,
            gen_id,
            message_snd: message_tx,
            command_task,
            event_tasks: HashMap::new(),
            event_handlers,
            timer_tasks: HashMap::new(),
            next_timer_id: 0,
            mailbox_poll_interval,
            mailbox_batch_size,
        };

        (msg_handler, message_rx, ready_rcv)
    }

    /// Returns a sink the BLE backend uses to deliver callbacks into this cell's
    /// message loop.
    #[cfg(feature = "ble-linux")]
    pub(crate) fn ble_sink(&self) -> BleCallbackSink {
        BleCallbackSink {
            snd: self.message_snd.clone(),
            sri: self.sri,
        }
    }

    /// Subscribe to an event - spawns a task that listens and forwards events
    pub(crate) fn subscribe_event(&mut self, event: Event) -> Result<()> {
        if !self.event_handlers.contains(event.as_ref()) {
            tracing::error!("bailing on subscribe");
            bail!(
                "cell {id} tried to subscribe to event for which it lacks a handler: {e}",
                id = self.sri,
                e = event.as_ref()
            );
        }

        if self.event_tasks.contains_key(event.as_ref()) {
            tracing::debug!(
                "cell {id} tried to repeatedly subscribe to event {e}",
                id = self.sri,
                e = event.as_ref()
            );
            return Ok(());
        }

        let key = event.as_ref().to_owned();
        let task = {
            let snd = self.message_snd.clone();
            let session = self.session.clone();
            let sri = self.sri;
            let poll_interval = self.mailbox_poll_interval;
            let batch_size = self.mailbox_batch_size;

            tokio::spawn(async move {
                if let Err(e) =
                    event_listener(session, sri, event, snd, poll_interval, batch_size).await
                {
                    tracing::error!("event listener error: {e}");
                }
            })
        };

        self.event_tasks.insert(key, task);
        Ok(())
    }

    /// Maximum number of concurrent timers per cell. Just a chosen number for now
    const MAX_TIMERS: usize = 5;

    /// Creates a timer that sends `TimerTick` messages on a schedule.
    /// Returns the timer ID, or an error if the per-cell limit is exceeded.
    pub(crate) fn create_timer(&mut self, request: CreateTimerRequest) -> Result<u32> {
        if request.count == Some(0) {
            bail!("timer count of 0 is invalid — timer would never fire");
        }
        if self.timer_tasks.len() >= Self::MAX_TIMERS {
            bail!(
                "timer limit exceeded: cell already has {} active timers (max {})",
                self.timer_tasks.len(),
                Self::MAX_TIMERS,
            );
        }

        let id = self.next_timer_id;
        self.next_timer_id += 1;

        let snd = self.message_snd.clone();
        let CreateTimerRequest {
            export_name,
            delay_ms,
            period_ms,
            count,
            fixed_delay,
        } = request;

        let handle = tokio::spawn(async move {
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }

            let mut remaining = count;
            loop {
                let (completed_tx, completed_rx) = if fixed_delay {
                    let (tx, rx) = oneshot::channel();
                    (Some(tx), Some(rx))
                } else {
                    (None, None)
                };

                let incoming = IncomingMessage {
                    span_context: None,
                    queued_at: None,
                    message: CellMessage::TimerTick(CellTimerTick {
                        timer_id: id,
                        export_name: export_name.clone(),
                        completed: completed_tx,
                    }),
                };
                if snd.send(incoming).await.is_err() {
                    break;
                }
                if let Some(completed_rx) = completed_rx {
                    let _ = completed_rx.await;
                }

                if let Some(ref mut n) = remaining {
                    *n -= 1;
                    if *n == 0 {
                        break;
                    }
                }

                if period_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(period_ms)).await;
                } else {
                    break;
                }
            }

            // notify the cell task that this timer completed naturally
            let _ = snd
                .send(IncomingMessage {
                    span_context: None,
                    message: CellMessage::TimerFinished(CellTimerFinished { timer_id: id }),
                    queued_at: None,
                })
                .await;
        });

        self.timer_tasks.insert(id, handle.abort_handle());
        Ok(id)
    }

    /// Returns whether a timer with the given ID is still active.
    pub(crate) fn is_timer_active(&self, id: u32) -> bool {
        self.timer_tasks.contains_key(&id)
    }

    /// Removes a finished timer from the registry. Called when a
    /// `TimerFinished` message is processed.
    pub(crate) fn remove_finished_timer(&mut self, id: u32) {
        self.timer_tasks.remove(&id);
    }

    /// Cancels an active timer. Returns an error if the ID is not found.
    pub(crate) fn cancel_timer(&mut self, id: u32) -> Result<()> {
        let Some(handle) = self.timer_tasks.remove(&id) else {
            bail!("timer {id} not found");
        };
        handle.abort();
        Ok(())
    }
}

impl Drop for CellMessageHandler {
    fn drop(&mut self) {
        // Free this cell's signal-layer claim so the next cell can take it
        // (swarm#1340); a no-op if this cell never claimed or a later
        // incarnation now owns it.
        crate::wasm::host_functions::release_sl_claim(self.sri, self.gen_id);
        for (_, task) in self.event_tasks.drain() {
            task.abort();
        }
        for (_, handle) in self.timer_tasks.drain() {
            handle.abort();
        }
        self.command_task.abort();
    }
}

// Internal: listens for events and forwards to channel
async fn event_listener(
    session: Session,
    sri: Sri,
    event: Event,
    snd: mpsc::Sender<IncomingMessage>,
    poll_interval: Duration,
    batch_size: usize,
) -> Result<()> {
    let mut queue = cell_mailbox::Mailbox::new(&session)
        .events(event)
        .await
        .map_err(|err| custom_err!("unable to subscribe to event: {err}"))?;

    // captured before `sri` is shadowed by a `KeyValue` below.
    let cell_sri = sri.to_string();

    let meter = opentelemetry::global::meter("cell_interaction");
    // named "queued" rather than "processed": this fires once an event is pulled off the
    // subscription and handed to the cell's message channel, not once the cell has actually run
    // it — see `cell_events_processed`, emitted from the cell task itself once the handler
    // completes.
    let events_queued = meter.u64_counter("cell_events_queued").build();
    let kind = KeyValue::new("kind", "event");
    let pid = KeyValue::new("pid", std::process::id().to_string());
    let sri = KeyValue::new("sri", sri.to_string());

    'event_processing: loop {
        // Covers one full loop iteration — the DB poll and every event's dispatch it produced —
        // as a single span, with one child span per event (see below) rather than a flat list.
        // Not linked into any call's distributed trace (unlike `cell_task::message_handler`):
        // this is for inspecting the batch loop's own local behavior, not the benchmark's
        // per-call latency numbers. Entered manually (rather than via `.instrument()`, which
        // re-enters/exits on every poll) around each phase separately — wrapping the DB poll in
        // an `.instrument()`-held span reproducibly kept it from ever being exported; this
        // mirrors how `cell_task::message_handler` (`observability::begin_observability`) holds
        // its span across its own multi-await handler execution.
        let batch_span = tracing::info_span!("cell_task::event_batch", sri = %cell_sri);

        let entered = batch_span.enter();
        let events = queue.receive_batch(poll_interval, batch_size);
        drop(entered);
        let events = events
            .await
            .map_err(|err| custom_err!("unable to receive event: {err}"))?;

        let entered = batch_span.enter();
        tracing::info!(count = events.len(), "received event batch");
        drop(entered);

        let mut stop = false;
        for event in events {
            let event_name = event.event.as_ref().to_owned();
            let attributes = &[
                KeyValue::new("event", event_name.clone()),
                kind.clone(),
                pid.clone(),
                sri.clone(),
            ];
            events_queued.add(1, attributes);

            let entered = batch_span.enter();
            let dispatch_span =
                tracing::info_span!("cell_task::event_dispatch", event = %event_name);
            let dispatch_entered = dispatch_span.enter();
            drop(dispatch_entered);
            drop(entered);

            let MailboxEvent {
                event,
                payload,
                attachment,
            } = event;

            let event = CellEvent {
                event,
                payload,
                sender: attachment.sender(),
            };

            let msg = IncomingMessage {
                span_context: attachment.span_context(),
                message: CellMessage::Event(event),
                queued_at: None,
            };

            let sent = snd.send(msg).await;
            drop(dispatch_span);

            if sent.is_err() {
                tracing::warn!("cell message receiver dropped");
                stop = true;
                break;
            }
        }
        drop(batch_span);

        if stop {
            break 'event_processing;
        }
    }

    Ok(())
}
