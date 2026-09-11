use std::collections::HashMap;

use cell_protocol::{EVENTS_TABLE, MailboxEvent, NAMESPACE_CELLS};
use db_client::v1::Subscription;
use db_commons::models::{Cursor, Scope, Subject, events};
use uuid::Uuid;

use crate::args::Ctx;
use crate::cmd::telemetry::debug::data::{DebugEvent, DebugItem, DebugPayload, insertion_time};
use crate::cmd::telemetry::debug::mailbox;

pub(crate) struct EventSubscriber {
    _subscription: Subscription,
}

impl EventSubscriber {
    pub(crate) async fn new(
        ctx: Ctx,
        db: db_client::v1::Client,
        tx: tokio::sync::mpsc::Sender<DebugItem>,
    ) -> anyhow::Result<Self> {
        let (sender, receiver) = tokio::sync::mpsc::channel::<(Scope, String)>(32);

        let collect_ctx = ctx.clone();
        let collect_db = db.clone();
        tokio::spawn(async move { data_collection(collect_ctx, collect_db, receiver, tx).await });

        let subscription = db
            .subscribe(
                Subject::Database(NAMESPACE_CELLS.into(), "@events".into()),
                EVENTS_TABLE,
                move |event| {
                    tokio::spawn(notification_handler(ctx.clone(), event, sender.clone()));
                },
            )
            .await
            .map_err(|err| anyhow::anyhow!("Failed to subscribe: {err}"))?;

        Ok(Self {
            _subscription: subscription,
        })
    }
}

async fn notification_handler(
    ctx: Ctx,
    notification: events::Notification,
    sender: tokio::sync::mpsc::Sender<(Scope, String)>,
) {
    if sender
        .send((notification.scope, notification.table))
        .await
        .is_err()
    {
        crate::debug!(&ctx, "dropping an event notification, collection has ended");
    }
}

async fn data_collection(
    ctx: Ctx,
    db: db_client::v1::Client,
    mut receiver: tokio::sync::mpsc::Receiver<(Scope, String)>,
    tx: tokio::sync::mpsc::Sender<DebugItem>,
) {
    let mut cursors = HashMap::<Scope, Cursor>::new();

    while let Some((scope, table)) = receiver.recv().await {
        // One failed read says nothing about the other scopes this one task serves, so the
        // cursor stays where it is and the next notification retries from it.
        let cursor = cursors.get(&scope).cloned();

        let response = match mailbox::list(&db, scope.clone(), table.clone(), cursor).await {
            Ok(response) => response,
            Err(err) => {
                crate::warn!(
                    &ctx,
                    "failed to read table '{table}' of scope '{}': {err}",
                    scope.database
                );

                continue;
            }
        };

        for (id, payload) in response.entities {
            let Some(inserted_at) = insertion_time(&id) else {
                continue;
            };
            cursors.insert(scope.clone(), Cursor::After(id));

            match postcard::from_bytes::<MailboxEvent>(&payload) {
                Ok(event) => {
                    let trace_id = event
                        .attachment
                        .span_context
                        .map(|span| Uuid::from_u128(span.trace_id()));
                    let debug_event = DebugEvent {
                        trace_id,
                        inserted_at,
                        event_name: event.event,
                        payload: DebugPayload::new(event.payload),
                    };

                    crate::debug!(
                        &ctx,
                        "captured event '{}' in scope '{}'",
                        debug_event.event_name.as_ref(),
                        scope.database
                    );

                    if tx.send(DebugItem::Event(debug_event)).await.is_err() {
                        crate::debug!(&ctx, "event collection ends, the writer is gone");

                        return;
                    }
                }
                Err(err) => {
                    crate::warn!(&ctx, "failed to parse a mailbox event: {err}");
                }
            }
        }
    }

    crate::debug!(&ctx, "event collection ends, no more notifications");
}
