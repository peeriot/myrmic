//! Sending side of the mailbox: build a command/event and insert it into the
//! right db table.

use std::sync::LazyLock;

use cell_protocol::{
    CellAttachment, EVENTS_TABLE, MESSAGES_TABLE, MailboxCommand, MailboxEvent, RuntimeId,
    SerializableSpanContext, Sri, scope_of_cell, scope_of_event,
};
use db_client::Session;
use db_client::application::Application;
use db_client::v1::models::{Operation, TxId};
use myrmic_common::cells::{Command, Event};
use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;

use crate::error::{Error, Result, to_bytes};

struct OutgoingMetrics {
    commands_sent: Counter<u64>,
    events_sent: Counter<u64>,
}

// Safety: OTel Counter<u64> is Send + Sync.
static METRICS: LazyLock<OutgoingMetrics> = LazyLock::new(|| {
    let meter = opentelemetry::global::meter("cell_interaction");
    OutgoingMetrics {
        commands_sent: meter.u64_counter("cell_commands_sent").build(),
        events_sent: meter.u64_counter("cell_events_sent").build(),
    }
});

enum MessageKind {
    Command(Sri, MailboxCommand),
    Event(MailboxEvent),
}

impl MessageKind {
    fn attachment_mut(&mut self) -> &mut CellAttachment {
        match self {
            Self::Command(_, v) => &mut v.attachment,
            Self::Event(v) => &mut v.attachment,
        }
    }
}

/// A fully-constructed outgoing message ready to be decorated and sent.
///
/// Construction methods encode domain knowledge (topic, attachment shape,
/// which db table to use). `attach_span_context` / `attach_sender` add
/// cross-cutting metadata. `send` handles transport.
pub struct OutgoingMessage {
    kind: MessageKind,
    source_sri: Option<Sri>,
}

impl OutgoingMessage {
    /// Fire-and-forget command.
    pub fn command(sri: &Sri, command: &Command, payload: Option<Vec<u8>>) -> Result<Self> {
        let cmd = MailboxCommand {
            cmd: command.clone(),
            payload,
            attachment: CellAttachment::default(),
        };

        Ok(Self {
            kind: MessageKind::Command(*sri, cmd),
            source_sri: None,
        })
    }

    /// Event publication.
    pub fn event(event: &Event, payload: Option<Vec<u8>>) -> Result<Self> {
        let payload = payload.unwrap_or_default();
        let attachment = CellAttachment::default();
        let event = MailboxEvent {
            event: event.clone(),
            payload,
            attachment,
        };
        Ok(Self {
            kind: MessageKind::Event(event),
            source_sri: None,
        })
    }

    /// Attach distributed tracing context to the outgoing message.
    pub fn attach_span_context<C>(&mut self, span_context: Option<C>)
    where
        C: Into<SerializableSpanContext>,
    {
        self.kind.attachment_mut().span_context = span_context.map(Into::into);
    }

    /// Stamp the identity of the cell emitting this message. `None` for
    /// messages that originate outside a cell (CLI, gateway).
    pub fn attach_sender(&mut self, sender: Option<uuid::Uuid>) {
        self.kind.attachment_mut().set_sender(sender);
    }

    /// Attribute this message's sent-metrics to the emitting cell, via a `"sri"` attribute
    /// on `cell_commands_sent`/`cell_events_sent` — distinct from [`Self::attach_sender`],
    /// which stamps the sender onto the message's own attachment for the receiver rather than
    /// onto the sender's own metrics.
    pub fn attach_source_sri(&mut self, source_sri: Sri) {
        self.source_sri = Some(source_sri);
    }

    /// Send from a raw session (builds a short-lived db client).
    pub async fn send(self, session: &Session, tx_id: Option<TxId>) -> Result<()> {
        let db = db_client::v1::Client::new(session);
        self.send_via_db(&db, tx_id).await
    }

    /// Send via an existing db client. If `tx_id` is provided the insert joins
    /// that transaction; otherwise it opens and commits its own.
    pub async fn send_via_db(self, db: &db_client::v1::Client, tx_id: Option<TxId>) -> Result<()> {
        use db_client::v1::models;

        let runtime_id = RuntimeId::from(db.zid());
        let op = self.into_op(runtime_id)?;

        let result = if let Some(tx_id) = tx_id {
            db.send(op.at(tx_id))
                .await
                .map(|reply| reply.map(drop).map_err(|err| err.message))
        } else {
            // One routed round trip instead of begin/insert/commit.
            let scope = op.scope.clone();
            db.send(models::tx_apply::Request::commit_new(
                models::tx_begin::Constraint::Routed(scope),
                vec![op.into()],
            ))
            .await
            .map(|reply| reply.map(drop).map_err(|err| err.message))
        };

        result
            .map_err(|err| Error::comm("send message", err))?
            .map_err(|message| Error::db("send message", message))?;

        Ok(())
    }

    /// Buffers delivery into an application, so the message lands with whatever
    /// else that transaction is doing — no round trip of its own.
    pub fn defer_into(self, application: &mut Application) -> Result<()> {
        let runtime_id = RuntimeId::from(application.client().zid());
        let op = self.into_op(runtime_id)?;

        application
            .defer(op)
            .map_err(|err| Error::db("send message", err.message().to_string()))
    }

    /// The table write that delivers this message: the mailbox row for a
    /// command, the event-bus row for an event. The id is minted server-side
    /// and never read back, which is what lets delivery be deferred.
    fn into_op(self, runtime_id: RuntimeId) -> Result<db_client::v1::models::tb_append::Op> {
        let source_sri = self.source_sri;
        let runtime_id = KeyValue::new("runtime_id", runtime_id.to_string());

        let (scope, table, payload) = match self.kind {
            MessageKind::Command(sri, command) => {
                let mut attributes = vec![
                    KeyValue::new("target_sri", sri.to_string()),
                    KeyValue::new("kind", "command"),
                    KeyValue::new("cmd", command.cmd.as_ref().to_string()),
                    KeyValue::new("mode", "async"),
                    runtime_id,
                ];
                if let Some(source_sri) = source_sri {
                    attributes.push(KeyValue::new("sri", source_sri.to_string()));
                }
                METRICS.commands_sent.add(1, &attributes);

                let payload = to_bytes(&command, "serialise command")?;

                (scope_of_cell(sri), String::from(MESSAGES_TABLE), payload)
            }
            MessageKind::Event(event) => {
                let mut attributes = vec![
                    KeyValue::new("kind", "event"),
                    KeyValue::new("event", event.event.as_ref().to_owned()),
                    runtime_id,
                ];
                if let Some(source_sri) = source_sri {
                    attributes.push(KeyValue::new("sri", source_sri.to_string()));
                }
                METRICS.events_sent.add(1, &attributes);

                let payload = to_bytes(&event, "serialise event")?;

                (
                    scope_of_event(event.event.as_ref()),
                    String::from(EVENTS_TABLE),
                    payload,
                )
            }
        };

        tracing::debug!("inserting into: {} // {}", scope, table);

        Ok(db_client::v1::models::tb_append::Op {
            scope,
            table,
            eid: None,
            value: payload,
        })
    }
}
