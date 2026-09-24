//! Plugin providing the capabilities to onboard to the swarm network where it is loaded.

use std::time::Duration;

use crate::plugins::{MyrmicCtx, deserialize_optional_duration};
use tokio::runtime::Runtime;
use tokio::sync::oneshot;

mod run;

pub struct SwarmOnboardingPlugin;

const DEFAULT_TIMEOUT: Duration = Duration::from_mins(5);

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct Config {
    /// How long one onboarding may run before it is abandoned and the next
    /// request is served. Humantime duration string. Defaults to 5 minutes
    /// when omitted.
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub timeout: Option<Duration>,
}

impl crate::plugins::MyrmicPlugin for SwarmOnboardingPlugin {
    const DEFAULT_NAME: &'static str = "onboarding";

    type Config = Config;

    async fn main(ctx: MyrmicCtx, config: Self::Config) -> zenoh::Result<()> {
        let (poison_snd, poison_rcv) = oneshot::channel();
        let session = ctx.session().clone();
        let timeout = config.timeout.unwrap_or(DEFAULT_TIMEOUT);

        // The onboarding process is spawned in a Tokio `LocalSet` via a dedicated runtime because
        // rustc cannot derive `Send` for futures internally using the `zenoh_traits` crate
        // (see `run.rs` for details), and `SimplePlugin::main` is required to return a `Send`
        // future.
        std::thread::spawn(move || {
            let rt = Runtime::new().expect("failed to create onboarding tokio runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, run::run(session, timeout, poison_rcv));
        });

        ctx.notify_ready();

        let _ = ctx.drop_notifier().recv_async().await;
        let _ = poison_snd.send(());
        Ok(())
    }
}
