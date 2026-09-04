//! Plugin providing the capabilities to onboard to the swarm network where it is loaded.

use crate::plugins::MyrmicCtx;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;

mod run;

pub struct SwarmOnboardingPlugin;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct Config {}

impl crate::plugins::MyrmicPlugin for SwarmOnboardingPlugin {
    const DEFAULT_NAME: &'static str = "onboarding";

    type Config = Config;

    async fn main(ctx: MyrmicCtx, _config: Self::Config) -> zenoh::Result<()> {
        let (poison_snd, poison_rcv) = oneshot::channel();
        let session = ctx.session().clone();

        // The onboarding process is spawned in a Tokio `LocalSet` via a dedicated runtime because
        // rustc cannot derive `Send` for futures internally using the `zenoh_traits` crate
        // (see `run.rs` for details), and `SimplePlugin::main` is required to return a `Send`
        // future.
        std::thread::spawn(move || {
            let rt = Runtime::new().expect("failed to create onboarding tokio runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, run::run(session, poison_rcv));
        });

        ctx.notify_ready();

        let _ = ctx.drop_notifier().recv_async().await;
        let _ = poison_snd.send(());
        Ok(())
    }
}
