use std::collections::HashMap;

use cell_protocol::{MESSAGES_TABLE, MailboxCommand, NAMESPACE_CELLS, Sri};
use db_client::v1::Subscription;
use db_commons::models::{Cursor, Scope, Subject, events, tb_list};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::cmd::telemetry::debug::data::{DebugCommand, DebugItem, DebugPayload, insertion_time};

pub(crate) struct MessageSubscriber {
    _subscription: Subscription,
    _handle: JoinHandle<anyhow::Result<()>>,
}

impl MessageSubscriber {
    pub(crate) async fn new(
        db: db_client::v1::Client,
        tx: tokio::sync::mpsc::Sender<DebugItem>,
    ) -> anyhow::Result<Self> {
        let (sender, receiver) = tokio::sync::mpsc::channel::<(Scope, String)>(32);

        let collect_db = db.clone();
        let handle = tokio::spawn(async move { data_collection(collect_db, receiver, tx).await });

        let subscription = db
            .subscribe(
                Subject::Namespace(NAMESPACE_CELLS.into()),
                MESSAGES_TABLE,
                move |event| {
                    tokio::spawn(notification_handler(event, sender.clone()));
                },
            )
            .await
            .map_err(|err| anyhow::anyhow!("Failed to subscribe: {err}"))?;

        Ok(Self {
            _subscription: subscription,
            _handle: handle,
        })
    }
}

async fn notification_handler(
    notification: events::Notification,
    sender: tokio::sync::mpsc::Sender<(Scope, String)>,
) {
    if let Err(err) = sender.send((notification.scope, notification.table)).await {
        eprintln!("{err}");
    }
}

async fn data_collection(
    db: db_client::v1::Client,
    mut receiver: tokio::sync::mpsc::Receiver<(Scope, String)>,
    tx: tokio::sync::mpsc::Sender<DebugItem>,
) -> anyhow::Result<()> {
    let mut cursors = HashMap::<Scope, Cursor>::new();

    while let Some((scope, table)) = receiver.recv().await {
        let cursor = cursors.get(&scope).cloned();
        let Ok(receiver_sri) = scope.database.parse::<Sri>() else {
            continue;
        };
        let response = query(&db, scope.clone(), table, cursor.clone()).await?;

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
                        .map(|ctx| Uuid::from_u128(ctx.trace_id()));
                    let payload = command.payload.map(DebugPayload::new);
                    let debug_command = DebugCommand {
                        trace_id,
                        inserted_at,
                        receiver_sri,
                        cmd: command.cmd,
                        payload,
                    };

                    tx.send(DebugItem::Command(debug_command)).await?;
                }
                Err(err) => {
                    eprintln!("Failed to parse mailbox command: {err}");
                }
            }
        }
    }

    Ok(())
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
