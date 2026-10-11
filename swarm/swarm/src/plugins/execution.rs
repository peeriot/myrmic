use crate::plugins::MyrmicCtx;
use sorg_common::{ExecConfig, PLUGIN_NAME_EXEC};
use tracing::debug;

pub struct SorgExecutionPlugin;

impl crate::plugins::MyrmicPlugin for SorgExecutionPlugin {
    const DEFAULT_NAME: &'static str = PLUGIN_NAME_EXEC;

    type Config = ExecConfig;

    async fn main(ctx: MyrmicCtx, config: Self::Config) -> zenoh::Result<()> {
        debug!("spawning sorg execution");
        let spawned = sorg_execution::spawn(
            ctx.session().clone(),
            config,
            ctx.tags().clone(),
            ctx.drop_notifier(),
            ctx.ready(),
        );
        spawned.await?;
        debug!("sorg execution terminated");

        Ok(())
    }
}
