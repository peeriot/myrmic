use self::{config::Config, run::fallible_run};
use crate::plugins::MyrmicCtx;
use zenoh::Result as ZResult;

mod config;
mod liveliness;
mod metrics;
mod run;

pub struct IntrospectionPlugin {}

impl crate::plugins::MyrmicPlugin for IntrospectionPlugin {
    const DEFAULT_NAME: &'static str = "introspection";

    type Config = Config;

    async fn main(ctx: MyrmicCtx, config: Self::Config) -> ZResult<()> {
        fallible_run(
            ctx.configs().clone(),
            ctx.session().clone(),
            ctx.drop_notifier(),
            config,
            ctx.ready(),
        )
        .await?;
        Ok(())
    }
}
