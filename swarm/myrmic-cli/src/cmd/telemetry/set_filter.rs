use swarm_telemetry::EnvFilter;
use zenoh::Session;

use crate::args::Ctx;

pub async fn handle(ctx: Ctx, filter: &str, session: &Session) -> anyhow::Result<()> {
    let _validated = EnvFilter::try_new(filter)?;

    session
        .put(swarm_telemetry::TOPIC_ENV_FILTER, filter)
        .await
        .map_err(|e| anyhow::anyhow!("failed to publish env_filter: {e}"))?;
    crate::info!(&ctx, "filter set to '{}' on all connected nodes", filter);
    Ok(())
}
