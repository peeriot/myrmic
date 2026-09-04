use crate::args::Ctx;
use crate::info;
use anyhow::bail;

#[derive(clap::Parser)]
pub struct Publish {
    /// Name of the event.
    name: String,

    /// Optional payload published with the event. Parsed as JSON by default; a
    /// value that isn't valid JSON is sent as a JSON string. Use `--raw` to
    /// send hex-decoded raw bytes instead.
    payload: Option<String>,

    /// Decode the payload as a hex string (optional `0x` prefix) and send the
    /// raw bytes as-is, bypassing JSON encoding. For non-JSON wire formats.
    #[clap(long)]
    raw: bool,
}

pub async fn handle(ctx: Ctx, cmd: Publish) -> anyhow::Result<()> {
    let Publish { name, payload, raw } = cmd;

    let payload = payload
        .map(|p| crate::payload::encode(p, raw))
        .transpose()?;

    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    let trace = swarm_telemetry::GeneratedTrace::default();
    info!(ctx, "trace ID = {trace}");
    let outcome = client
        .publish_cell_event_trace(&name, payload, Some(trace.as_tuple()))
        .await;

    match outcome {
        Ok(()) => info!(ctx, "published!"),
        Err(err) => bail!("command failed: {err}"),
    }

    Ok(())
}
