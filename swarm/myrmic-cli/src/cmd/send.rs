use crate::args::Ctx;
use crate::info;
use anyhow::bail;
use cell_protocol::Sri;

#[derive(clap::Parser)]
pub struct Send {
    /// The SRI or SRN of the target cell the command is delivered to.
    #[clap(value_name = "SRI/SRN")]
    id: String,

    /// Name of the command to invoke on the target cell.
    name: String,

    /// Optional payload passed to the command. Parsed as JSON by default; a
    /// value that isn't valid JSON is sent as a JSON string. Use `--raw` to
    /// send hex-decoded raw bytes instead.
    payload: Option<String>,

    /// Decode the payload as a hex string (optional `0x` prefix) and send the
    /// raw bytes as-is, bypassing JSON encoding. For non-JSON wire formats.
    #[clap(long)]
    raw: bool,
}

pub async fn handle(ctx: Ctx, cmd: Send) -> anyhow::Result<()> {
    let Send {
        id,
        name,
        payload,
        raw,
    } = cmd;

    let payload = payload
        .map(|p| crate::payload::encode(p, raw))
        .transpose()?;

    // Resolve the target at the edge: a UUID literal is taken as an SRI
    // verbatim, anything else is an SRN path derived to its SRI. The network
    // only ever sees the UUID.
    let sri = match Sri::from_target(&id) {
        Ok(sri) => sri,
        Err(e) => bail!("invalid target '{id}': {e}"),
    };

    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    // we pre-generate a trace ID that we display here and inject as span context for all
    // following calls
    let trace = swarm_telemetry::GeneratedTrace::default();
    info!(ctx, "trace ID = {trace}");

    let outcome = client
        .command_send(sri, &name, payload, Some(trace.as_tuple()))
        .await;

    match outcome {
        Ok(()) => info!(ctx, "successfully sent command"),
        Err(err) => bail!("command failed: {err}"),
    }

    Ok(())
}
