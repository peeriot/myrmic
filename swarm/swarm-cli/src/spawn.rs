pub fn handle(cmd: &crate::cmd::Spawn) -> anyhow::Result<()> {
    let swarm = swarm::Swarm::from_path(&cmd.config)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let result = rt.block_on(async move {
        let mut spawned = swarm.wait_in_place().await?;

        let result = tokio::select! {
            _ = tokio::signal::ctrl_c() => Ok(()),
            err = spawned.plugin_failure() => Err(err),
        };

        tracing::info!("Shutting down");

        spawned.kill_async().await;

        result
    });

    let graceful_shutdown = cmd.graceful_shutdown;

    tracing::info!("Killing runtime ({})...", graceful_shutdown);

    rt.shutdown_timeout(graceful_shutdown.into());

    tracing::info!("Killed!");

    result
}
