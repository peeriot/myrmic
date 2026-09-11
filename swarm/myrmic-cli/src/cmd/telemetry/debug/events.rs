use std::collections::HashMap;

use cell_protocol::{EVENTS_TABLE, MailboxEvent, NAMESPACE_CELLS};
use db_client::v1::Subscription;
use db_commons::models::{Cursor, Scope, Subject, events, tb_count, tb_list};
use uuid::Uuid;

use crate::args::Ctx;
use crate::cmd::telemetry::debug::data::{DebugEvent, DebugItem, DebugPayload, insertion_time};

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
        let cursor = match cursors.get(&scope) {
            Some(cursor) => cursor.clone(),
            None => {
                // no cursor for this scope yet, we are doing a best effort job here to not query
                // all events from the DB but rather hope events are not firing so fast that the
                // assumption of new 1 event per processed notification stays true for at least
                // the first event in that scope.
                match count(&db, scope.clone(), table.clone()).await {
                    Ok(response) => Cursor::Skip(response.count - 1),
                    Err(err) => {
                        crate::warn!(
                            &ctx,
                            "failed to count table '{table}' of scope '{}': {err}",
                            scope.database
                        );

                        continue;
                    }
                }
            }
        };

        let response = match query(&db, scope.clone(), table.clone(), cursor).await {
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

async fn query(
    db: &db_client::v1::Client,
    scope: Scope,
    table: String,
    cursor: Cursor,
) -> anyhow::Result<tb_list::Response> {
    db.read_tx_in(scope.clone(), async move |client, tx_id| {
        let req = tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope,
                table,
                cursor: Some(cursor),
                limit: None,
                order: None,
            },
        };

        Ok(client
            .send(req)
            .await?
            .map_err(|err| anyhow::anyhow!("{}", err.message))?)
    })
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))
}

async fn count(
    db: &db_client::v1::Client,
    scope: Scope,
    table: String,
) -> anyhow::Result<tb_count::Response> {
    db.read_tx_in(scope.clone(), async move |client, tx_id| {
        let req = tb_count::Request {
            id: tx_id,
            op: tb_count::Op { scope, table },
        };

        Ok(client
            .send(req)
            .await?
            .map_err(|err| anyhow::anyhow!("{}", err.message))?)
    })
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))
}
