//! Plugin collecting and transforming embedded log messages into normal traces

use crate::plugins::MyrmicCtx;

mod run;

pub struct EmbeddedLoggingPlugin;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct Config {}

impl crate::plugins::MyrmicPlugin for EmbeddedLoggingPlugin {
    const DEFAULT_NAME: &'static str = "embedded-logging";

    type Config = Config;

    async fn main(ctx: MyrmicCtx, _config: Self::Config) -> zenoh::Result<()> {
        ctx.notify_ready();
        run::run(ctx.session().clone(), ctx.drop_notifier()).await;
        Ok(())
    }
}
