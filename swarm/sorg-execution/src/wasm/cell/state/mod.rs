//! Module defining the structs that represent the information that the host maintains about cells

use std::collections::HashSet;

use cell_mailbox::OutgoingMessage;
use cell_protocol::Gen;
use cell_protocol::Sri;
use cell_protocol::scope_of_cell;
use db_client::application::{self, Application};
use db_client::v1::models::{Deferrable, Operation, TxId};
use opentelemetry::trace::SpanContext;
use sorg_common::{SpawnLineage, bail, custom_err};
use tokio::sync::{mpsc, oneshot};
use zenoh::Session;

use crate::wasm::cell::observability;
use crate::{
    Result,
    wasm::cell::{CellMessage, IncomingMessage},
};

pub(crate) use commands::DropHandle;
pub(crate) use message_handler::CellMessageHandler;

#[cfg(feature = "ble-linux")]
pub(crate) use ble::CellBle;

#[cfg(feature = "ble-linux")]
mod ble;
mod commands;
mod message_handler;

pub(crate) struct CellState {
    /// The cell's identity — handed to its Wasm handlers and stamped as the
    /// sender on messages it emits. Also its routable address / DB scope key.
    sri: Sri,
    /// Incarnation minted at deploy admission; anchors children spawned by
    /// this cell and identifies this instance against its registry row.
    gen_id: Gen,
    /// Spawn-time lineage (parent identity + incarnation, detachment, local
    /// name) — consulted when this cell spawns, stops, or is reported lost.
    lineage: SpawnLineage,
    session: Session,
    stored_arguments: Option<Vec<u8>>,
    err_msg: Option<String>,
    /// The application every host call made from the currently running cell
    /// function joins: writes nobody reads back buffer in it, a call that needs
    /// a value flushes them with itself last. Opened lazily by the first host
    /// call that needs one and committed when the function returns, so a
    /// function's writes land as one unit — and a function that never touches
    /// the db costs nothing at all.
    application: Option<Application>,
    msg_handler: CellMessageHandler,
    /// Span of the message currently being processed, and the trace context
    /// stamped onto any messages the cell emits while handling it. Set in
    /// [`begin_message`](Self::begin_message), cleared when the message
    /// finishes. Transparent to cell logic (observability only).
    current_span: tracing::Span,
    current_span_context: Option<SpanContext>,
    /// Targets whose placement this cell has already verified. A send's
    /// pre-flight `ensure_placement_exists` is a routed read — a locate gather
    /// plus the lookup — paid inside the handler's dispatch, and at rack load
    /// it was most of a sender's per-command wall clock (run 33463140540:
    /// dispatch mean 1.4ms quiet to 7.3ms at load 1000 around a 13µs handler,
    /// while central, which sends nothing, sustained ~5x the senders' rate).
    /// The check only ever asserted "a placement row existed on the freshest
    /// replica just now", so a cached positive narrows no guarantee that
    /// mattered; negatives are never cached, so a not-yet-placed target is
    /// re-checked on the next send.
    verified_targets: HashSet<Sri>,
    #[cfg(feature = "ble-linux")]
    ble: CellBle,
}

impl CellState {
    pub(crate) fn state_and_msg_rcv(
        sri: Sri,
        gen_id: Gen,
        lineage: SpawnLineage,
        session: Session,
        event_handlers: HashSet<String>,
        mailbox_poll_interval: std::time::Duration,
        mailbox_batch_size: usize,
    ) -> (Self, mpsc::Receiver<IncomingMessage>, oneshot::Receiver<()>) {
        let (msg_handler, msg_rcv, ready_rcv) = CellMessageHandler::new(
            session.clone(),
            sri,
            gen_id,
            event_handlers,
            mailbox_poll_interval,
            mailbox_batch_size,
        );
        #[cfg(feature = "ble-linux")]
        let ble = CellBle::new(msg_handler.ble_sink());
        let state = Self {
            sri,
            gen_id,
            lineage,
            session,
            stored_arguments: None,
            err_msg: None,
            application: None,
            msg_handler,
            current_span: tracing::Span::none(),
            current_span_context: None,
            verified_targets: HashSet::new(),
            #[cfg(feature = "ble-linux")]
            ble,
        };
        (state, msg_rcv, ready_rcv)
    }

    /// Begins processing an inbound message: opens its observability span,
    /// records the trace context for outbound propagation, and hands back the
    /// inner [`CellMessage`] to dispatch. Paired with `finish_message` in the
    /// cell runtime loop.
    #[track_caller]
    pub(crate) fn begin_message(&mut self, incoming: IncomingMessage) -> CellMessage {
        let module_id = self.sri.to_string();
        let (span, span_context) = observability::begin_observability(&incoming, &module_id);
        self.current_span = span;
        self.current_span_context = span_context;
        incoming.message
    }

    /// Stamps cross-cutting metadata (trace context + this cell's identity as
    /// the sender) onto a message the cell is about to emit. Callers just say
    /// "send this"; they don't know what is attached.
    pub(crate) fn decorate_outgoing(&self, msg: &mut OutgoingMessage) {
        msg.attach_span_context(self.current_span_context.clone());
        msg.attach_sender(Some(self.sri.as_uuid()));
        msg.attach_source_sri(self.sri);
    }

    /// Takes the current message's span, clearing it from state so it can be
    /// closed exactly once when the message finishes.
    pub(crate) fn take_current_span(&mut self) -> tracing::Span {
        std::mem::replace(&mut self.current_span, tracing::Span::none())
    }

    pub(crate) fn sri(&self) -> &Sri {
        &self.sri
    }

    pub(crate) fn gen_id(&self) -> Gen {
        self.gen_id
    }

    pub(crate) fn lineage(&self) -> &SpawnLineage {
        &self.lineage
    }

    /// The running cell function's application, opened on first use. Whoever
    /// dispatched the function closes it via
    /// [`take_application`](Self::take_application).
    ///
    /// Routed to the cell's own slice as a hint: the guest names the scope of
    /// each db call at the moment it makes it, and one function can touch its
    /// private slice, a public namespace, its mailbox and another cell's inbox,
    /// so no exact scope is knowable here. `Routed` is a placement hint, not a
    /// boundary.
    pub(crate) fn application(&mut self) -> &mut Application {
        if self.application.is_none() {
            let client = db_client::v1::Client::new(&self.session);
            self.application = Some(Application::routed(client, scope_of_cell(self.sri)));
        }

        self.application
            .as_mut()
            .expect("an application was just opened")
    }

    /// Buffers an operation nobody reads back into the function's application.
    ///
    /// Fails once an earlier operation has aborted the function's transaction:
    /// there is nothing left for the write to join.
    pub(crate) fn defer<T: Deferrable>(&mut self, op: T) -> application::Result<()> {
        self.application().defer(op)
    }

    /// Applies an operation whose value the caller needs, flushing anything
    /// deferred before it.
    pub(crate) async fn apply<T: Operation>(&mut self, op: T) -> application::Result<T::Response> {
        self.application().apply(op).await
    }

    /// The transaction of the running function, for the host calls that still
    /// name one themselves. Flushes anything deferred, so program order holds.
    pub(crate) async fn transaction(&mut self) -> Result<TxId> {
        let id = self
            .application()
            .tx_id()
            .await
            .map_err(|err| custom_err!("unable to begin a transaction: {err}"))?;

        Ok(id)
    }

    /// Surrenders the function's application to the caller, which is then
    /// responsible for committing or abandoning it.
    pub(crate) fn take_application(&mut self) -> Option<Application> {
        self.application.take()
    }

    /// Whether a send to `sri` has already passed the placement check — see
    /// the `verified_targets` field.
    pub(crate) fn target_verified(&self, sri: &Sri) -> bool {
        self.verified_targets.contains(sri)
    }

    pub(crate) fn mark_target_verified(&mut self, sri: Sri) {
        // A cell addressing unboundedly many targets (a gateway fanning out
        // per session) must not grow this forever; re-verifying after a reset
        // is just the first-send cost again.
        if self.verified_targets.len() >= 4096 {
            self.verified_targets.clear();
        }
        self.verified_targets.insert(sri);
    }

    pub(crate) fn subscribe_event(&mut self, event: myrmic_common::cells::Event) -> Result<()> {
        self.msg_handler.subscribe_event(event)
    }

    pub(crate) fn create_timer(
        &mut self,
        request: myrmic_common::cells::CreateTimerRequest,
    ) -> Result<u32> {
        self.msg_handler.create_timer(request)
    }

    pub(crate) fn cancel_timer(&mut self, id: u32) -> Result<()> {
        self.msg_handler.cancel_timer(id)
    }

    pub(crate) fn is_timer_active(&self, id: u32) -> bool {
        self.msg_handler.is_timer_active(id)
    }

    pub(crate) fn remove_finished_timer(&mut self, id: u32) {
        self.msg_handler.remove_finished_timer(id);
    }

    /// Returns the current observability span, if any. Used by host functions to
    /// enter the span briefly so their tracing events inherit the trace context.
    // Only referenced by logging code that is temporarily commented out; kept
    // because it will be needed again once that code is re-enabled.
    #[allow(dead_code)]
    pub(crate) fn current_span(&self) -> tracing::Span {
        self.current_span.clone()
    }

    pub(crate) fn take_err_msg(&mut self) -> Option<String> {
        self.err_msg.take()
    }

    pub(crate) fn set_err_msg(&mut self, msg: String) {
        self.err_msg = Some(msg);
    }

    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    /// Can be used directly, but also acts as plumbing for more helpful functions like `take_arguments`, etc
    fn args(&mut self) -> &mut Option<Vec<u8>> {
        &mut self.stored_arguments
    }

    /// Provide the bytes the host stored for the currently processed event/command
    pub(crate) fn take_arguments(&mut self) -> Option<Vec<u8>> {
        self.args().take()
    }

    /// Stores the input for the command/event which is to be processed next
    pub(crate) fn store_arguments(&mut self, input: Vec<u8>) -> Result<()> {
        let args = self.args();
        if args.is_some() {
            bail!("found unused input");
        }
        *args = Some(input);
        Ok(())
    }

    pub(crate) fn clear_arguments(&mut self) {
        self.args().take();
    }

    /// The cell's per-cell BLE backend handle.
    #[cfg(feature = "ble-linux")]
    pub(crate) fn ble(&self) -> &CellBle {
        &self.ble
    }
}

impl crate::wasm::host_functions::CellIdentity for CellState {
    fn sl_identity(&self) -> Option<(cell_protocol::Sri, Gen)> {
        Some((*self.sri(), self.gen_id()))
    }
}
