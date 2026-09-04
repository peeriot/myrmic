pub fn handle(cmd: &crate::cmd::Spawn) -> anyhow::Result<()> {
    let swarm = swarm::Swarm::from_path(&cmd.config)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        let spawned = swarm.wait_in_place().await.unwrap();

        let _ = tokio::signal::ctrl_c().await.ok();

        tracing::info!("Shutting down");

        spawned.kill_async().await;
    });

    let graceful_shutdown = cmd.graceful_shutdown;

    tracing::info!("Killing runtime ({})...", graceful_shutdown);

    rt.shutdown_timeout(graceful_shutdown.into());

    tracing::info!("Killed!");

    Ok(())
}
