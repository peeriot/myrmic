use std::future::Future;

use db_commons::models::Cursor;

use crate::args::Ctx;
use crate::cmd::telemetry::debug::data::DebugItem;
use crate::cmd::telemetry::debug::stream::DebugStream;

mod data;
mod events;
mod logs;
mod mailbox;
mod messages;
mod stream;

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

    let stream = async {
        // the types are spelled out so that dropping a `?` below is a compile error rather than
        // a binding that silently holds an unchecked `Result` and drops the subscription
        let _message_subscriber: messages::MessageSubscriber =
            messages::MessageSubscriber::new(ctx.clone(), db.clone(), tx_debug.clone()).await?;
        let _event_subscriber: events::EventSubscriber =
            events::EventSubscriber::new(ctx.clone(), db.clone(), tx_debug).await?;

        // Anchored before the log subscription so a row inserted in between is still greater than
        // the anchor and still comes back on the first query; the other order would drop it.
        let log_cursor = match logs::newest_row_id(&db).await? {
            Some(id) => Some(Cursor::After(id)),
            None => {
                crate::warn!(
                    &ctx,
                    "No log records stored yet; did you set a retention period? (ie `myrmic telemetry set-db-retention 1h`)"
                );

                None
            }
        };

        let _log_subscriber: logs::LogSubscriber =
            logs::LogSubscriber::new(ctx.clone(), db.clone(), tx_log).await?;

        let writer = debug_writer(
            &ctx,
            db,
            rx_debug,
            rx_log,
            cmd.json,
            sri_filter.as_deref(),
            log_cursor,
        );
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

        anyhow::Ok(())
    };

    run_then_restore(
        stream,
        restore_cell_log_level(&ctx, &session, restore_filter),
    )
    .await
}

/// Awaits `restore` whatever `stream` ended with - nothing else puts a filter raised by
/// `--level` back on the connected nodes.
async fn run_then_restore(
    stream: impl Future<Output = anyhow::Result<()>>,
    restore: impl Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let streamed = stream.await;
    let restored = restore.await;

    streamed.and(restored)
}

/// Puts `filter` back on all connected nodes. `None` means the level was never raised.
async fn restore_cell_log_level(
    ctx: &Ctx,
    session: &zenoh::Session,
    filter: Option<String>,
) -> anyhow::Result<()> {
    let Some(filter) = filter else {
        return Ok(());
    };

    session
        .put(swarm_telemetry::TOPIC_ENV_FILTER, &filter)
        .await
        .map_err(|err| anyhow::anyhow!("failed to restore env_filter: {err}"))?;
    crate::info!(ctx, "restored filter to '{filter}'");

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
    ctx: &Ctx,
    db: db_client::v1::Client,
    mut rx_dbg: tokio::sync::mpsc::Receiver<DebugItem>,
    mut rx_log: tokio::sync::mpsc::Receiver<()>,
    json: bool,
    sri_filter: Option<&str>,
    log_cursor: Option<Cursor>,
) -> anyhow::Result<()> {
    let mut stream = DebugStream::new(log_cursor);

    // zenoh pub/sub gives no signal when the swarm goes away - the subscribers above just fall
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
                        stream.push(item);
                    }
                    None => break,
                }
            }
            // once a new log batch was inserted we are collecting relevant logs for each debug
            // item (trace ID) and bring the data in timely order to actually print it
            _ = rx_log.recv() => {
                // One failed read says nothing about the next notification, and the cursor stays
                // where it is, so the rows this one missed come back on the following read. The
                // ping above is what ends the loop when the swarm is really gone.
                let response = match logs::query(&db, stream.log_cursor()).await {
                    Ok(response) => response,
                    Err(err) => {
                        crate::warn!(ctx, "failed to read the log table: {err}");

                        continue;
                    }
                };

                if response.entities.is_empty() {
                    // the batch that triggered this notification didn't contain any logs at all.
                    // print everything already queued.
                    for item in stream.drain_all(sri_filter) {
                        print_item(&item, json)?;
                    }
                }

                for (id, payload) in response.entities {
                    stream.advance(&id);

                    let Some((target, record)) = logs::parse(ctx, &payload) else {
                        continue;
                    };

                    // debug is about cell logs specifically - drop anything else client-side,
                    // regardless of what the remote EnvFilter let through
                    if !logs::is_cell_target(target.as_deref()) {
                        continue;
                    }

                    // filter log for SRI
                    let sri = logs::sri(&record);
                    match (sri, sri_filter) {
                        (Some(sri), Some(filter)) if sri.as_str() != filter => continue,
                        _ => {}
                    }

                    // logs come back in chronological (id) order, so once we've reached one at
                    // or after a queued item's own timestamp, that item's window is closed -
                    // print it now, before the log line.
                    for item in stream.flush_before(&id, sri_filter) {
                        print_item(&item, json)?;
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

#[cfg(test)]
mod tests {
    use super::run_then_restore;

    #[tokio::test]
    async fn a_stream_that_failed_is_still_followed_by_the_restore() {
        let mut restored = false;

        let result = run_then_restore(async { anyhow::bail!("failed to subscribe") }, async {
            restored = true;

            Ok(())
        })
        .await;

        assert_eq!(
            result
                .expect_err("the stream's failure ends the command")
                .to_string(),
            "failed to subscribe"
        );
        assert!(restored);
    }

    #[tokio::test]
    async fn a_restore_that_failed_ends_the_command_with_its_error() {
        let result = run_then_restore(async { Ok(()) }, async {
            anyhow::bail!("failed to restore env_filter")
        })
        .await;

        assert_eq!(
            result
                .expect_err("a filter left raised is reported")
                .to_string(),
            "failed to restore env_filter"
        );
    }
}
