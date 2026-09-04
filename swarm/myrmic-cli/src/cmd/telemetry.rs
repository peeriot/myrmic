use std::fmt::Debug;

use db_client::v1::models::tb_list;
use serde::Serialize;
use serde::de::DeserializeOwned;
use swarm_telemetry::db::ScopedEntry;
use uuid::Uuid;

use crate::args::Ctx;

mod debug;
mod logs;
mod metrics;
mod set_db_retention;
mod set_filter;
mod traces;

#[derive(clap::Parser)]
pub struct Telemetry {
    #[clap(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Show logs produced by swarm
    Logs(Logs),
    /// Export traces in [Trace Event Format](https://docs.google.com/document/d/1CvAClvFfyA5R-PhYUmn5OOQtYMH4h6I0nSsKchNAySU/preview?tab=t.0#heading=h.yr4qxyxotyw)
    /// that is readable by '<chrome://tracing>' or [Perfetto](https://ui.perfetto.dev/).
    Traces(Traces),
    /// Insight into the current state of the swarm
    Metrics(Metrics),
    /// Change the log filter of all running swarm nodes at runtime.
    ///
    /// Publishes the new filter string to all connected nodes. The filter syntax
    /// follows the `tracing` crate's `EnvFilter` format, e.g. `"info"` or
    /// `"debug,h2=warn,zenoh=off"`.
    SetFilter(SetFilter),
    /// Persist telemetry to the DB, keeping it for the given period.
    ///
    /// Telemetry is only written to the DB while a retention period is set;
    /// without one (the default) nothing is persisted.
    SetDbRetention(SetDbRetention),
    /// Stop persisting telemetry to the DB (the default).
    NoDbRetention,
    /// Run in debug mode
    ///
    /// Creates a stream (on stdout) of commands and events optionally (with telemetry feature) enriched with
    /// log information.
    Debug(debug::Debug),
}

#[derive(clap::Parser)]
pub struct TraceIdFilter {
    /// Filter by trace ID
    #[clap(short, long)]
    trace_id: Option<Uuid>,
}

#[derive(clap::Parser)]
pub struct Logs {
    #[command(flatten)]
    trace_id_filter: TraceIdFilter,
}

#[derive(clap::Parser)]
pub struct Traces {
    #[command(flatten)]
    trace_id_filter: TraceIdFilter,
}

#[derive(clap::Parser)]
pub struct Metrics {}

#[derive(clap::Parser)]
pub struct SetFilter {
    /// New filter string, e.g. `"info"` or `"debug,h2=warn,zenoh=off"`.
    filter: String,
}

#[derive(clap::Parser)]
pub struct SetDbRetention {
    /// in humantime format, e.g. 15days 2min 2s
    retention: String,
}

pub async fn handle(ctx: Ctx, cmd: Telemetry) -> anyhow::Result<()> {
    let session = ctx.session().await?;
    let db_client = || db_client::v1::Client::new(&session);

    match cmd.cmd {
        Cmd::Logs(logs) => logs::handle(ctx, logs, db_client()).await,
        Cmd::Traces(traces) => traces::handle(ctx, traces, db_client()).await,
        Cmd::Metrics(metrics) => metrics::handle(ctx, metrics, db_client()).await,
        Cmd::SetFilter(config) => set_filter::handle(ctx, &config.filter, &session).await,
        Cmd::SetDbRetention(config) => {
            set_db_retention::handle(ctx, &config.retention, &session).await
        }
        Cmd::NoDbRetention => set_db_retention::handle(ctx, "null", &session).await,
        Cmd::Debug(cmd) => debug::handle(ctx, cmd).await,
    }
}

async fn query_telemetry_data<T>(
    db_client: db_client::v1::Client,
    table: impl Into<String>,
) -> anyhow::Result<Vec<(Uuid, ScopedEntry<T>)>>
where
    T: Serialize + DeserializeOwned + Debug,
{
    db_client
        .read_tx_in(swarm_telemetry::db::scope(), async move |client, id| {
            let list_req = tb_list::Request {
                id,
                op: tb_list::Op {
                    cursor: None,
                    limit: None,
                    order: None,
                    scope: swarm_telemetry::db::scope(),
                    table: table.into(),
                },
            };

            client.send(list_req).await
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to comminicate with DB: {err}"))?
        .map_err(|err| anyhow::anyhow!("unable to query telemetry data from DB: {}", err.message))?
        .entities
        .into_iter()
        .map(|(id_bytes, data)| {
            let id_array = id_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("ID is not 16 bytes"))?;
            let id = Uuid::from_bytes(id_array);
            let entry = serde_json::from_slice::<ScopedEntry<T>>(&data)?;
            Ok((id, entry))
        })
        .collect::<anyhow::Result<Vec<_>>>()
}
