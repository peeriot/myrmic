use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use cell_protocol::{EVENTS_TABLE, MailboxEvent, NAMESPACE_CELLS, Sri, scope_of_event};
use db_commons::models::{Cursor, Scope, Subject, tb_count, tb_list};
use myrmic_common::cells::Event;
use tokio::time::MissedTickBehavior;

use crate::args::Ctx;
use crate::{info, warn};

// Best-effort backstop; promptness comes from the poke subscription.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(clap::Parser)]
pub struct Subscribe {
    /// Event names to log. Accepts multiple names and/or comma-separated
    /// lists (`myrmic sub a b` and `myrmic sub a,b` are equivalent).
    /// With no names, every event on the network is logged.
    #[clap(value_delimiter = ',')]
    filter: Vec<String>,
}

pub async fn handle(ctx: Ctx, cmd: Subscribe) -> anyhow::Result<()> {
    let mut filters = Vec::with_capacity(cmd.filter.len());
    for name in cmd.filter {
        let event = Event::try_from(name.as_str())
            .map_err(|err| anyhow::anyhow!("invalid event name {name:?}: {err}"))?;
        filters.push(event);
    }

    let session = ctx.session().await?;
    let db = db_client::v1::Client::new(&session);

    let subjects: Vec<Subject> = if filters.is_empty() {
        vec![Subject::Database(NAMESPACE_CELLS.into(), "@events".into())]
    } else {
        filters
            .iter()
            .map(|event| Subject::Scope(scope_of_event(event.as_ref())))
            .collect()
    };

    // Pokes only say "something landed in this scope"; the cursor map below
    // is what actually reads the events out of the table.
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Scope>(64);

    let mut subscriptions = Vec::with_capacity(subjects.len());
    for subject in subjects {
        let sender = sender.clone();
        let subscription = db
            .subscribe(subject, EVENTS_TABLE, move |notification| {
                // A dropped poke is fine; the backstop interval catches up.
                let _ = sender.try_send(notification.scope);
            })
            .await
            .map_err(|err| anyhow::anyhow!("unable to subscribe to events: {err}"))?;
        subscriptions.push(subscription);
    }

    // Named events start with their cursor past everything already stored,
    // so only events published from here on are logged.
    let mut cursors = HashMap::<Scope, Cursor>::new();
    for event in &filters {
        let scope = scope_of_event(event.as_ref());
        let count = count(&db, scope.clone()).await?;
        cursors.insert(scope, Cursor::Skip(count));
    }

    if filters.is_empty() {
        info!(ctx, "subscribed to all events (ctrl-c to stop)");
    } else {
        let names = filters
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>()
            .join(", ");
        info!(ctx, "subscribed to: {names} (ctrl-c to stop)");
    }

    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        let poked = tokio::select! {
            scope = receiver.recv() => scope,
            _ = interval.tick() => None,
        };

        let scopes: Vec<Scope> = match poked {
            Some(scope) => vec![scope],
            // Backstop tick: catch up every scope we've seen, in case a poke
            // was dropped or never delivered.
            None => cursors.keys().cloned().collect(),
        };

        for scope in scopes {
            if let Err(err) = drain_scope(ctx, &db, &mut cursors, scope).await {
                warn!(ctx, "{err:#}");
            }
        }
    }
}

/// Log everything past `scope`'s cursor, advancing it.
async fn drain_scope(
    ctx: Ctx,
    db: &db_client::v1::Client,
    cursors: &mut HashMap<Scope, Cursor>,
    scope: Scope,
) -> anyhow::Result<()> {
    let cursor = match cursors.get(&scope) {
        Some(cursor) => cursor.clone(),
        None => {
            // First sighting of this event name: the poke that got us here is
            // the event we want, so back up one from the current count. Best
            // effort — if several arrived at once we only see the last.
            let count = count(db, scope.clone()).await?;
            let cursor = Cursor::Skip(count.saturating_sub(1));
            cursors.insert(scope.clone(), cursor.clone());
            cursor
        }
    };

    let entities = list(db, scope.clone(), cursor).await?;

    for (id, payload) in entities {
        cursors.insert(scope.clone(), Cursor::After(id.clone()));

        match postcard::from_bytes::<MailboxEvent>(&payload) {
            Ok(event) => print_event(&id, &event),
            Err(err) => warn!(ctx, "skipping undecodable event in {scope}: {err}"),
        }
    }

    Ok(())
}

/// Header line, then the payload: pretty-printed if it parses as JSON,
/// base64 otherwise.
fn print_event(id: &[u8], event: &MailboxEvent) {
    let sender = match event.attachment.sender() {
        Some(uuid) => Sri::from(uuid).to_string(),
        None => String::from("external"),
    };

    let timestamp = insertion_time(id)
        .map(|time| format!("[{}] ", humantime::format_rfc3339_millis(time)))
        .unwrap_or_default();

    println!();
    println!(
        "{timestamp}event={} sender={sender} payload={} bytes",
        event.event.as_ref(),
        event.payload.len(),
    );

    if event.payload.is_empty() {
        return;
    }

    match serde_json::from_slice::<serde_json::Value>(&event.payload) {
        Ok(value) => match serde_json::to_string_pretty(&value) {
            Ok(pretty) => println!("{pretty}"),
            Err(_) => println!("{value}"),
        },
        Err(_) => println!(
            "{}",
            base64::engine::general_purpose::STANDARD.encode(&event.payload)
        ),
    }
}

/// Insertion time from a row id (a `UUIDv7` for events), `None` otherwise.
fn insertion_time(id: &[u8]) -> Option<SystemTime> {
    let uuid = uuid::Uuid::from_slice(id).ok()?;
    let ts = uuid.get_timestamp()?;
    let (secs, nanos) = ts.to_unix();
    Some(SystemTime::UNIX_EPOCH + Duration::new(secs, nanos))
}

async fn count(db: &db_client::v1::Client, scope: Scope) -> anyhow::Result<usize> {
    let response = db
        .read_tx_in(scope.clone(), async move |client, tx_id| {
            Ok(client
                .send(tb_count::Request {
                    id: tx_id,
                    op: tb_count::Op {
                        scope,
                        table: String::from(EVENTS_TABLE),
                    },
                })
                .await?
                .map_err(|err| anyhow::anyhow!("{}", err.message))?)
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to count events: {err}"))?;

    Ok(response.count)
}

async fn list(
    db: &db_client::v1::Client,
    scope: Scope,
    cursor: Cursor,
) -> anyhow::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let response = db
        .read_tx_in(scope.clone(), async move |client, tx_id| {
            Ok(client
                .send(tb_list::Request {
                    id: tx_id,
                    op: tb_list::Op {
                        scope,
                        table: String::from(EVENTS_TABLE),
                        cursor: Some(cursor),
                        limit: None,
                        order: None,
                    },
                })
                .await?
                .map_err(|err| anyhow::anyhow!("{}", err.message))?)
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to list events: {err}"))?;

    Ok(response.entities)
}
