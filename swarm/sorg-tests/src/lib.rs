//! Crate for code used within the tests of the sorg-related crates
#![allow(clippy::missing_panics_doc)]

use std::{collections::HashMap, sync::Arc, time::Duration};

use sorg_client::Client;
use tokio::{
    sync::{
        Mutex,
        mpsc::{UnboundedSender, unbounded_channel},
    },
    task::JoinHandle,
};
use zenoh::{Session, config::ZenohId, key_expr::OwnedKeyExpr};
mod cells;
mod embedded;
mod fs;
mod http_mock;
mod log_tracker;
mod mqtt;
mod pub_sub;
mod queryables;
mod runtimes;
mod swarm;
mod system_state;
mod wasm;

pub use cells::EventQueue;
pub use embedded::{DeployResponseMode, MockEmbeddedExec};
// Re-exported so the embedded mock's `Target`-based API is usable without a
// direct `myrmic-tags` dependency in consumer test crates.
pub use fs::load_into_db;
pub use http_mock::HttpMockHandle;
pub use log_tracker::{StateTracker, TaskStatus, set_up_log_tracker};
pub use myrmic_tags::Platform;
pub use swarm::{
    KillableProcess, ScopedProcess, scope_test_multicast, set_up_killable_swarm,
    set_up_swarm_with_config, test_session,
};
pub use wasm::{build_and_register_cell_class, build_cell};

pub use tracing_subscriber;

const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
pub const WAIT_TIME: Duration = Duration::from_millis(10);

type SharedMap<T> = Arc<Mutex<HashMap<T, Vec<Vec<u8>>>>>;

/// Initializes logging for testing purposes using the provided log level or filter arguments.
/// This (a) allows to log within tests and (b) will display the logs of the tested component.
///
/// # Arguments
///
/// * `log_args` - A string slice specifying the logging filter, e.g., `"info"` or `"my_crate=debug"`.
///
pub fn enable_test_logging(log_args: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(log_args)
        .with_test_writer()
        .try_init()
        .ok();
}

const INIT_RETRIES: usize = 50;
const INIT_PROBE_INTERVAL: Duration = Duration::from_millis(50);

type HealthResponseSnd = std::sync::mpsc::Sender<bool>;

pub struct TestApp {
    pub query_timeout: Duration,
    sorg_client: Client,
    received_msgs: SharedMap<OwnedKeyExpr>,
    received_mqtt_msgs: SharedMap<String>,
    received_queries: SharedMap<OwnedKeyExpr>,
    pub swarm_handle: ScopedProcess,
    health_check_handle: JoinHandle<()>,
    health_sender: UnboundedSender<HealthResponseSnd>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let service_alive_at_the_end = self.probe_tested_service();
        self.health_check_handle.abort();

        if !service_alive_at_the_end {
            if std::thread::panicking() {
                // we are already panicking -> just write an error msg
                eprintln!("Tested service not reachable when test app dropped");
            } else {
                // no panic yet -> panic so that the test fails, since we would expect the service to stick around
                panic!("Tested service not reachable when test app dropped");
            }
        }
    }
}

impl TestApp {
    // Gets the path of the swarm config (relative to the 'data' folder in the respective integrations test)
    // and a function which can be called to check whether the tested service is available
    pub async fn spawn<H, F>(swarm_handle: ScopedProcess, health_check: H) -> Self
    where
        H: Fn() -> F + Send + Sync + 'static,
        F: std::future::Future<Output = bool> + Send + 'static,
    {
        // Check that the service is up before doing anything else
        for attempt in 1..=INIT_RETRIES {
            let service_up = health_check().await;
            if service_up {
                break;
            }

            assert!(
                attempt < INIT_RETRIES,
                "tested service not up after {INIT_RETRIES} x {INIT_PROBE_INTERVAL:?}"
            );

            tokio::time::sleep(INIT_PROBE_INTERVAL).await;
        }

        // Set up the task which will let us check health in the drop
        let (snd, mut rcv) = unbounded_channel::<HealthResponseSnd>();
        let health_check_task = tokio::spawn(async move {
            while let Some(rsp_snd) = rcv.recv().await {
                let service_up = health_check().await;
                let _ = rsp_snd.send(service_up);
            }
        });

        let session = swarm_handle.session();

        let mut sorg_client_config = sorg_client::Config::default();
        sorg_client_config.set_query_timeout(QUERY_TIMEOUT);
        let sorg_client = Client::new_with_config(session.clone(), sorg_client_config);

        Self {
            sorg_client,
            query_timeout: QUERY_TIMEOUT,
            received_msgs: SharedMap::default(),
            received_mqtt_msgs: SharedMap::default(),
            received_queries: SharedMap::default(),
            swarm_handle,
            health_check_handle: health_check_task,
            health_sender: snd,
        }
    }

    #[must_use]
    pub fn probe_tested_service(&self) -> bool {
        let (rtx, rrx) = std::sync::mpsc::channel();
        if self.health_sender.send(rtx).is_err() {
            return false; // task died -> treat as unhealthy
        }
        rrx.recv().unwrap_or(false) // no reply/timeout -> unhealthy
    }

    /// Represents the ID of the runtime on which the tested functionality is deployed
    #[must_use]
    pub fn runtime_id(&self) -> ZenohId {
        self.session().zid()
    }

    #[must_use]
    pub fn session(&self) -> &Session {
        self.swarm_handle.session()
    }
}

#[macro_export]
macro_rules! data_file {
    ($file_name: expr) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/", $file_name)
    };
}
