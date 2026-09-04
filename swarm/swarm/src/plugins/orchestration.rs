use crate::plugins::MyrmicCtx;
use sorg_common::PLUGIN_NAME_ORCH;
use tracing::{debug, error};

mod config;

use self::config::Config;

pub struct SorgOrchestrationPlugin;

impl crate::plugins::MyrmicPlugin for SorgOrchestrationPlugin {
    const DEFAULT_NAME: &'static str = PLUGIN_NAME_ORCH;

    type Config = Config;

    async fn main(ctx: MyrmicCtx, config: Self::Config) -> zenoh::Result<()> {
        debug!("spawning sorg orchestration");
        let spawned = sorg_orchestration::spawn(
            ctx.session().clone(),
            config.into(),
            ctx.drop_notifier(),
            ctx.ready(),
        );
        match spawned.await {
            Ok(()) => debug!("sorg orchestration terminated"),
            Err(err) => error!("sorg orchestration terminated with an error: {err}"),
        }
        Ok(())
    }
}
