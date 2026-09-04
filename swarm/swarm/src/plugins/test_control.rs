mod run;

use crate::plugins::MyrmicCtx;
use test_control_common::PLUGIN_NAME_TEST_CTRL;

pub struct ZenohTestControlPlugin;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct Config {}

impl crate::plugins::MyrmicPlugin for ZenohTestControlPlugin {
    const DEFAULT_NAME: &'static str = PLUGIN_NAME_TEST_CTRL;

    type Config = Config;

    async fn main(ctx: MyrmicCtx, _config: Self::Config) -> zenoh::Result<()> {
        ctx.notify_ready();
        run::run(ctx.session().clone(), ctx.drop_notifier()).await;
        Ok(())
    }
}
