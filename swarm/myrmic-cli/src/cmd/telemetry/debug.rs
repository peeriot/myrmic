use std::collections::BTreeSet;

use db_commons::models::Cursor;

use crate::{args::Ctx, cmd::telemetry::debug::data::DebugItem};

mod data;
mod events;
mod logs;
mod messages;

#[derive(clap::Parser)]
pub struct Debug {
    #[clap(long)]
    /// print JSON instead of human readable lines
    pub json: bool,
    #[clap(long)]
    /// stop debugging after timeout
    pub timeout: Option<humantime::Duration>,
    #[clap(long, value_name = "SRI/SRN")]
    /// only show entries for this cell (SRI or SRN)
    pub id: Option<String>,
    #[clap(long)]
    /// temporarily change the log level for cell logs on all connected nodes, restoring their
    /// previous filter on exit. Leaves the remaining targets filter untouched.
    pub level: Option<String>,
}

pub async fn handle(ctx: Ctx, cmd: Debug) -> anyhow::Result<()> {
    // Stored entries carry the canonical SRI (UUID) string, so resolve the
    // filter target up front — this also lets an SRN match.
    let sri_filter = cmd
        .id
        .as_deref()
        .map(|target| {
            cell_protocol::Sri::from_target(target)
                .map(|sri| sri.to_string())
                .map_err(|e| anyhow::anyhow!("invalid target '{target}': {e}"))
        })
        .transpose()?;

    crate::info!(&ctx, "starting debug stream");

    let abort = tokio::signal::ctrl_c();
    let timeout = tokio::time::sleep(
        cmd.timeout
            .map_or_else(|| std::time::Duration::MAX, std::time::Duration::from),
    );

    let (tx_debug, rx_debug) = tokio::sync::mpsc::channel(128);
    let (tx_log, rx_log) = tokio::sync::mpsc::channel(8);

    let session = ctx.session().await?;
    let db = db_client::v1::Client::new(&session);

    let restore_filter = match &cmd.level {
        Some(level) => Some(raise_cell_log_level(&ctx, &session, level).await?),
        None => None,
    };

    let _message_subscriber = messages::MessageSubscriber::new(db.clone(), tx_debug.clone()).await;
    let _event_subscriber = events::EventSubscriber::new(db.clone(), tx_debug).await;
    let _log_subscriber = logs::LogSubscriber::new(db.clone(), tx_log).await;

    let writer = debug_writer(db, rx_debug, rx_log, cmd.json, sri_filter.as_deref());
    tokio::select! {
        _ = abort => {
            crate::info!(&ctx, "ctrl-c received");
        }
        () = timeout => {
            crate::info!(&ctx, "debugging ends after timeout");
        }
        res = writer => {
            if let Err(err) = res {
                crate::error!(&ctx, "debug writer exited unexpectedly: {err}");
            }
        }
    };

    if let Some(filter) = restore_filter {
        session
            .put(swarm_telemetry::TOPIC_ENV_FILTER, &filter)
            .await
            .map_err(|err| anyhow::anyhow!("failed to restore env_filter: {err}"))?;
        crate::info!(&ctx, "restored filter to '{filter}'");
    }

    Ok(())
}

/// Queries the baseline filter active on connected nodes, then overrides it with the baseline
/// plus cell-log directives raised to `level` — leaving everything else untouched so `OTel`
/// export for other targets is unaffected. Returns the baseline to restore on exit.
async fn raise_cell_log_level(
    ctx: &Ctx,
    session: &zenoh::Session,
    level: &str,
) -> anyhow::Result<String> {
    // right after `Ctx::session`, this process's zenoh peer may not have finished discovering
    // the other nodes on the network yet (scouting runs in the background and isn't complete
    // just because `zenoh::open` returned) — an empty reply set here doesn't necessarily mean no
    // node is out there, so retry for a bit before concluding that and giving up.
    let mut baselines = swarm_telemetry::query_env_filter(session).await;
    for _ in 0..9 {
        if !baselines.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        baselines = swarm_telemetry::query_env_filter(session).await;
    }

    let baseline = match baselines.as_slice() {
        [single] => single.clone(),
        [] => {
            anyhow::bail!(
                "no node answered the current filter — refusing to override it blindly, since \
                 we wouldn't know what to restore on exit"
            );
        }
        multiple => {
            anyhow::bail!(
                "connected nodes disagree on their current filter ({multiple:?}) — refusing to \
                 override it, since restoring afterwards would overwrite whichever nodes don't \
                 match the one we'd pick"
            );
        }
    };

    let overridden = format!("{baseline},{}", logs::level_override_directives(level));
    let _validated = swarm_telemetry::EnvFilter::try_new(&overridden)?;

    session
        .put(swarm_telemetry::TOPIC_ENV_FILTER, &overridden)
        .await
        .map_err(|err| anyhow::anyhow!("failed to raise cell log level: {err}"))?;
    crate::info!(
        &ctx,
        "raised cell log level to '{level}' (filter: '{overridden}')"
    );

    Ok(baseline)
}

fn print_item(item: &DebugItem, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string(item)?);
    } else {
        println!("{item}");
    }

    Ok(())
}

async fn debug_writer(
    db: db_client::v1::Client,
    mut rx_dbg: tokio::sync::mpsc::Receiver<DebugItem>,
    mut rx_log: tokio::sync::mpsc::Receiver<()>,
    json: bool,
    sri_filter: Option<&str>,
) -> anyhow::Result<()> {
    let mut queue = BTreeSet::<DebugItem>::new();
    let mut log_cursor: Option<Cursor> = None;

    // zenoh pub/sub gives no signal when the swarm goes away — the subscribers above just fall
    // silent forever. Periodically ping the swarm so a lost connection actually ends this loop
    // instead of hanging.
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(5));
    ping_interval.tick().await;

    loop {
        tokio::select! {
            _ = ping_interval.tick() => {
                if let Err(err) = db.ping().await {
                    anyhow::bail!("lost connection to the swarm: {err}");
                }
            }
            item = rx_dbg.recv() => {
                match item {
                    Some(item) => {
                        queue.insert(item);
                    }
                    None => break,
                }
            }
            // once a new log batch was inserted we are collecting relevant logs for each debug
            // item (trace ID) and bring the data in timely order to actually print it
            _ = rx_log.recv() => {
                // we read logs from the last cursor, if we have one. otherwise we are faking
                // a cursor by building a UUIDv7 from the timestamp of the first payload to
                // debug
                let query_cursor = match &log_cursor {
                    Some(log_cursor) => log_cursor.clone(),
                    None => {
                        if let Some(first) = queue.first() {
                            let millis: u64 = first
                                .timestamp()
                                .duration_since(std::time::UNIX_EPOCH)?
                                .as_millis()
                                .try_into()?;
                            let id = uuid::Builder::from_unix_timestamp_millis(millis, &[0u8; 10]).into_uuid();
                            Cursor::After(id.as_bytes().to_vec())
                        } else {
                            continue;
                        }
                    }
                };

                let response = logs::query(&db, Some(query_cursor)).await?;

                if response.entities.is_empty() {
                    // the batch that triggered this notification didn't contain any logs at all.
                    // print everything already queued.
                    while let Some(item) = queue.pop_first() {
                        print_item(&item, json)?;
                    }
                }
                for (id, payload) in response.entities {
                    log_cursor = Some(Cursor::After(id));

                    let Some((target, record)) = logs::parse(&payload) else {
                        continue;
                    };

                    // debug is about cell logs specifically — drop anything else client-side,
                    // regardless of what the remote EnvFilter let through
                    if !logs::is_cell_target(target.as_deref()) {
                        continue;
                    }

                    let record_time = logs::time(&record);

                    // filter log for SRI
                    let sri = logs::sri(&record);
                    match (sri, sri_filter) {
                        (Some(sri), Some(filter)) if sri.as_str() != filter => continue,
                        _ => {}
                    }


                    // logs come back in chronological (id) order, so once we've
                    // reached one at or after a queued item's own timestamp,
                    // that item's window is closed — print it now, before the
                    // log line, instead of in a separate pass over `queue`.
                    while queue.first().is_some_and(|item| *item.timestamp() <= record_time) {
                        let item = queue.pop_first().expect("just checked non-empty");
                        if item.filter_sri(sri_filter) {
                            print_item(&item, json)?;
                        }
                    }

                    if json {
                        let mut value = serde_json::to_value(&record)?;
                        if let (Some(target), Some(obj)) = (&target, value.as_object_mut()) {
                            obj.insert("target".into(), serde_json::Value::String(target.clone()));
                        }
                        println!("{value}");
                    } else {
                        println!("{}", logs::format(&record));
                    }
                }
            }
        }
    }

    Ok(())
}
