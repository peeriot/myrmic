use crate::plugins::MyrmicCtx;
use zenoh::Result as ZResult;

pub struct Plugin;

impl crate::plugins::MyrmicPlugin for Plugin {
    const DEFAULT_NAME: &'static str = "gateway";

    type Config = swarm_gateway::Config;

    async fn main(ctx: MyrmicCtx, config: Self::Config) -> ZResult<()> {
        let drop_rx = ctx.drop_notifier();

        swarm_gateway::run(config, ctx.session().clone(), ctx.ready(), async move {
            let _ = drop_rx.recv_async().await;
        })
        .await?;
        Ok(())
    }
}
