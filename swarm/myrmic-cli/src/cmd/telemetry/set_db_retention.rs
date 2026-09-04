use zenoh::Session;

use crate::args::Ctx;

pub async fn handle(ctx: Ctx, retention: &str, session: &Session) -> anyhow::Result<()> {
    session
        .put(swarm_telemetry::TOPIC_DB_RETENTION, retention)
        .await
        .map_err(|e| anyhow::anyhow!("failed to publish DB retention: {e}"))?;
    crate::info!(
        &ctx,
        "DB retention set to '{}' on all connected nodes",
        retention
    );
    Ok(())
}
