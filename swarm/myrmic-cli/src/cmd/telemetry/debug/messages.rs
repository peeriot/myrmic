use std::collections::HashMap;

use cell_protocol::{MESSAGES_TABLE, MailboxCommand, NAMESPACE_CELLS, Sri};
use db_client::v1::Subscription;
use db_commons::models::{Cursor, Scope, Subject, events, tb_list};
use uuid::Uuid;

use crate::args::Ctx;
use crate::cmd::telemetry::debug::data::{DebugCommand, DebugItem, DebugPayload, insertion_time};

pub(crate) struct MessageSubscriber {
    _subscription: Subscription,
}

impl MessageSubscriber {
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
                Subject::Namespace(NAMESPACE_CELLS.into()),
                MESSAGES_TABLE,
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
        crate::debug!(
            &ctx,
            "dropping a mailbox notification, collection has ended"
        );
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
        let cursor = cursors.get(&scope).cloned();
        let Ok(receiver_sri) = scope.database.parse::<Sri>() else {
            continue;
        };

        // One failed read says nothing about the other cells this one task serves, so the
        // cursor stays where it is and the next notification retries from it.
        let response = match query(&db, scope.clone(), table.clone(), cursor).await {
            Ok(response) => response,
            Err(err) => {
                crate::warn!(
                    &ctx,
                    "failed to read table '{table}' of cell {receiver_sri}: {err}"
                );

                continue;
            }
        };

        for (id, payload) in response.entities {
            let Some(inserted_at) = insertion_time(&id) else {
                continue;
            };
            cursors.insert(scope.clone(), Cursor::After(id));

            match postcard::from_bytes::<MailboxCommand>(&payload) {
                Ok(command) => {
                    let trace_id = command
                        .attachment
                        .span_context
                        .map(|span| Uuid::from_u128(span.trace_id()));
                    let payload = command.payload.map(DebugPayload::new);
                    let debug_command = DebugCommand {
                        trace_id,
                        inserted_at,
                        receiver_sri,
                        cmd: command.cmd,
                        payload,
                    };

                    crate::debug!(
                        &ctx,
                        "captured command '{}' for cell {receiver_sri}",
                        debug_command.cmd.as_ref()
                    );

                    if tx.send(DebugItem::Command(debug_command)).await.is_err() {
                        crate::debug!(&ctx, "command collection ends, the writer is gone");

                        return;
                    }
                }
                Err(err) => {
                    crate::warn!(&ctx, "failed to parse a mailbox command: {err}");
                }
            }
        }
    }

    crate::debug!(&ctx, "command collection ends, no more notifications");
}

async fn query(
    db: &db_client::v1::Client,
    scope: Scope,
    table: String,
    cursor: Option<Cursor>,
) -> anyhow::Result<tb_list::Response> {
    db.read_tx_in(scope.clone(), async move |client, tx_id| {
        let req = tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope,
                table,
                cursor,
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
