use crate::args::Ctx;

pub async fn handle(
    _ctx: Ctx,
    cmd: super::Traces,
    db_client: db_client::v1::Client,
) -> anyhow::Result<()> {
    let spans = super::query_telemetry_data(db_client, swarm_telemetry::db::TABLE_TRACES).await?;

    let mut events = swarm_telemetry::trace_event_format::process_spans(
        spans.into_iter(),
        cmd.trace_id_filter.trace_id,
    );

    // we order events by time and ensure that flows are "enclosed" by begin/end. so Ord impl
    // for details
    events.sort();

    serde_json::to_writer_pretty(std::io::stdout(), &events)?;

    Ok(())
}
