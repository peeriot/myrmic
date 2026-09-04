use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use cell_protocol::{RuntimeId, Sri};
use db_client::v1::{Client as DbClient, models::Scope};
use serde::de::DeserializeOwned;
use sorg_client::Client;
use sorg_common::{
    DeploymentError, ExecRuntimeInfo, RequirementTags, exec_registry::list_registered_execs,
    instance_registry, key_get, key_put, node_lease, supervision::SupervisionTiming,
};
use uuid::Uuid;
use zenoh::Session;

pub use sorg_client::EventQueue;

/// How long [`SorgHandle::connect`] / [`SorgHandle::connect_with_tags`] wait for a matching exec
/// runtime to appear in the registry with a fresh liveness lease (i.e. to become placeable).
pub const DEFAULT_EXEC_RUNTIME_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between exec-registry polls while waiting for a runtime.
const EXEC_RUNTIME_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Test assertion helpers on top of [`EventQueue`].
#[allow(async_fn_in_trait)]
pub trait EventQueueExt {
    /// Assert that the next `expected.len()` events arrive in order with the given payloads.
    async fn assert_ordered(&mut self, expected: &[&[u8]]);
}

impl EventQueueExt for EventQueue {
    async fn assert_ordered(&mut self, expected: &[&[u8]]) {
        let events = self
            .try_receive_batch(expected.len())
            .await
            .expect("failed to receive expected events");
        for (i, (got, want)) in events.iter().zip(expected.iter()).enumerate() {
            assert_eq!(*want, got.as_slice(), "event[{i}] payload mismatch");
        }
    }
}

/// A thin wrapper around sorg client that waits for an exec runtime
#[derive(Clone)]
pub struct SorgHandle {
    session: Session,
    sorg_client: Client,
    tags: RequirementTags,
}

impl SorgHandle {
    /// [`Self::connect_with_tags`] without any tag requirements.
    pub async fn connect(session: Session) -> Self {
        Self::connect_with_tags(session, &[]).await
    }

    /// [`Self::connect_with_tags_timeout`] with [`DEFAULT_EXEC_RUNTIME_TIMEOUT`].
    pub async fn connect_with_tags(session: Session, tags: &[&str]) -> Self {
        Self::connect_with_tags_timeout(session, tags, DEFAULT_EXEC_RUNTIME_TIMEOUT).await
    }

    /// Wait up to `timeout` for an exec runtime carrying all `tags` to be placeable on `session`
    /// (registered with a fresh liveness lease), then return a handle whose cell loads are scoped
    /// to those tags.
    pub async fn connect_with_tags_timeout(
        session: Session,
        tags: &[&str],
        timeout: Duration,
    ) -> Self {
        let req_tags =
            RequirementTags::new(tags.iter().map(std::string::ToString::to_string).collect());
        wait_for_exec_runtime(&session, tags, timeout).await;
        let sorg_client = Client::new(session.clone());
        Self {
            session,
            sorg_client,
            tags: req_tags,
        }
    }

    /// The underlying zenoh session.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Replace the sorg query timeout used for deploys.
    ///
    /// The client default ([`sorg_client::Config`]) is sized for host round-trips. Runtimes
    /// reached over a slow link — an embedded device that has to fetch the cell blob before it
    /// can answer — need a longer one.
    #[must_use]
    pub fn with_query_timeout(mut self, timeout: Duration) -> Self {
        let mut config = sorg_client::Config::default();
        config.set_query_timeout(timeout);
        self.sorg_client = Client::new_with_config(self.session.clone(), config);
        self
    }

    /// Wait up to `timeout` for a *further* exec runtime carrying all `tags`.
    ///
    /// [`Self::connect_with_tags`] already waits for the handle's own tags; use this for
    /// scenarios that deploy across more than one runtime (e.g. a device plus the swarm's own
    /// Linux exec) and so must know the second one has registered too.
    pub async fn wait_for_runtime(&self, tags: &[&str], timeout: Duration) {
        wait_for_exec_runtime(&self.session, tags, timeout).await;
    }

    /// Deploy a registered wasm class as a cell under `cell_sri`, scoped to this handle's tags.
    pub async fn load_cell(&self, class_name: &str, cell_sri: &str) {
        self.load_cell_with_tags(class_name, cell_sri, &self.tags.clone())
            .await;
    }

    /// [`Self::load_cell`] scoped to `tags` instead of the handle's own tags — for scenarios
    /// that place some cells on one runtime and some on another.
    pub async fn load_cell_with_tags(
        &self,
        class_name: &str,
        cell_sri: &str,
        tags: &RequirementTags,
    ) {
        self.try_load_cell_with_tags(class_name, cell_sri, tags)
            .await
            .expect("load_cell failed");
    }

    /// [`Self::load_cell_with_tags`] returning the orchestrator's error instead of panicking —
    /// for tests that assert on a deployment failure.
    pub async fn try_load_cell_with_tags(
        &self,
        class_name: &str,
        cell_sri: &str,
        tags: &RequirementTags,
    ) -> Result<(), DeploymentError> {
        self.try_load_cell_with_tags_and_args(class_name, cell_sri, tags, None)
            .await
    }

    /// [`Self::try_load_cell_with_tags`] delivering `arguments` to the cell's `#[init]` via the
    /// deployment command. `None` for cells whose init takes no payload.
    pub async fn try_load_cell_with_tags_and_args(
        &self,
        class_name: &str,
        cell_sri: &str,
        tags: &RequirementTags,
        arguments: Option<Vec<u8>>,
    ) -> Result<(), DeploymentError> {
        let sri = Sri::from_target(cell_sri).expect("invalid cell SRI/name");
        self.sorg_client
            .deploy_wasm_cell_with_arguments(sri, class_name, tags.clone(), arguments, None)
            .await
    }

    /// Undeploy the cell at `cell_sri`, leaving its persisted state and instance record intact.
    pub async fn undeploy_cell(&self, cell_sri: &str) {
        let sri = Sri::from_target(cell_sri).expect("invalid cell SRI/name");
        self.sorg_client
            .undeploy_cell(sri)
            .await
            .expect("undeploy_cell failed");
    }

    /// Create an instance record for `cell_sri`, optionally pre-seeding its persisted state under
    /// `key` (the same key the cell itself would use with `wasm_sdk::State::new_const(key)`), as a
    /// previous run of the cell would have left behind. The class must already be registered.
    ///
    /// synchronous/host-managed default cell state was removed system-wide in the jezza/sdk
    /// merge — persisted state is now always addressed by a caller-chosen key, so this seeds it
    /// through the same generic key/value store a deployed cell reads from via `State`.
    pub async fn create_instance(
        &self,
        cell_sri: &str,
        class_name: &str,
        key: &str,
        state: Vec<u8>,
    ) {
        let sri = Sri::of_path(cell_sri).expect("invalid cell sri");
        let record = cell_protocol::CellInstance {
            sri,
            class_name: class_name.to_owned(),
            gen_id: cell_protocol::Gen::from_parts(1, 1),
            lineage: cell_protocol::SpawnLineage::default(),
        };
        instance_registry::insert_registry_entry(self.session(), &record)
            .await
            .expect("create_instance failed");
        if !state.is_empty() {
            let scope = Scope::new(sri.to_string(), "d", "p");
            let db_client = DbClient::new(self.session());
            db_client
                .write_tx(async move |client, tx_id| {
                    key_put(client.clone(), tx_id, scope, key.to_owned(), state)
                        .await
                        .map_err(Into::into)
                })
                .await
                .expect("failed to seed cell state");
        }
    }

    /// Send a fire-and-forget command to the cell at `sri`, without waiting for it to run.
    pub async fn command_send(&self, sri: Sri, cmd_name: &str, payload: Option<Vec<u8>>) {
        self.sorg_client
            .command_send(sri, cmd_name, payload, None)
            .await
            .expect("command_send failed");
    }

    /// [`Self::command_send`], returning a fresh trace id generated for this call so it can be
    /// correlated with logs/spans recorded during the command's execution (e.g. via `myrmic
    /// telemetry logs --trace-id`) — without blocking on its outcome, since synchronous/callback
    /// command execution was removed system-wide in the jezza/sdk merge.
    pub async fn command_send_traced(
        &self,
        sri: Sri,
        cmd_name: &str,
        payload: Option<Vec<u8>>,
    ) -> Uuid {
        let trace_id = Uuid::new_v4();
        let (span_id, _) = trace_id.as_u64_pair();
        self.sorg_client
            .command_send(sri, cmd_name, payload, Some((trace_id.as_u128(), span_id)))
            .await
            .expect("command_send failed");
        trace_id
    }

    /// Read and deserialize the value the cell persisted under `key` (its default/private scope,
    /// matching `wasm_sdk::State::new_const(key)`). Returns `None` if nothing is stored yet.
    pub async fn get_cell_state<S: DeserializeOwned>(&self, sri: Sri, key: &str) -> Option<S> {
        let scope = Scope::new(sri.to_string(), "d", "p");
        let db_client = DbClient::new(self.session());
        let key = key.to_owned();
        let state_bytes = db_client
            .read_tx(async move |client, tx_id| {
                key_get(client.clone(), tx_id, scope, key)
                    .await
                    .map_err(Into::into)
            })
            .await
            .expect("failed to get cell state")?;
        Some(postcard::from_bytes(&state_bytes).expect("failed to deserialize state"))
    }

    /// Subscribe to a cell event topic; received payloads are queued on the returned [`EventQueue`].
    pub async fn subscribe_cell_event(&mut self, event: &str) -> EventQueue {
        self.sorg_client
            .subscribe_cell_event(event)
            .await
            .expect("subscribe_cell_event failed")
    }

    /// Publish a payload on a cell event topic, as a cell would.
    pub async fn publish_cell_event(&self, event: &str, payload: Vec<u8>) {
        self.sorg_client
            .publish_cell_event(event, Some(payload))
            .await
            .expect("publish_cell_event failed");
    }
}

async fn wait_for_exec_runtime(session: &Session, tags: &[&str], timeout: Duration) {
    let retries = (timeout.as_millis() / EXEC_RUNTIME_POLL_INTERVAL.as_millis()).max(1);
    let retries = u32::try_from(retries).unwrap_or(u32::MAX);

    tryhard::retry_fn(|| async {
        // `list_registered_execs` reads the same DB-backed exec registry the orchestrator
        // itself reads before placing cells (see `sorg_common::exec_registry`). A runtime's
        // own `TOPIC_EXEC_RUNTIMES` queryable is not equivalent: that queryable starts
        // answering as soon as the exec runtime's queryables are registered, which happens
        // *before* it registers itself in the DB-backed registry (see
        // `sorg-execution/src/spawn.rs`) — racing ahead of that registration let a
        // subsequent `load_cell` reach the orchestrator before it could see the runtime,
        // failing with `NoRuntimesAvailable`.
        let execs = list_registered_execs(session).await.map_err(|_| ())?;
        let fresh = fresh_lease_ids(session).await?;

        if execs
            .iter()
            .any(|info| runtime_has_tags(info, tags) && fresh.contains(&info.id()))
        {
            Ok(())
        } else {
            Err(())
        }
    })
    .retries(retries)
    .fixed_backoff(EXEC_RUNTIME_POLL_INTERVAL)
    .await
    .unwrap_or_else(|()| panic!("exec runtime with tags {tags:?} not available after {timeout:?}"));
}

fn runtime_has_tags(info: &ExecRuntimeInfo, tags: &[&str]) -> bool {
    tags.iter()
        .all(|t| info.capabilities().tags().iter().any(|c| c.as_ref() == *t))
}

/// The runtimes whose liveness lease is fresh enough that placement would keep them,
/// applying the same staleness rule as `drop_stale_execs` in the orchestrator: a node is
/// fresh while the silence since its last renewal (`seq`, wall-clock millis) stays within its
/// declared `ttl_ms` plus the cluster margin. A `seq` ahead of the reader's clock (skew)
/// counts as fresh, and a node with no lease row is simply absent from the set.
async fn fresh_lease_ids(session: &Session) -> Result<HashSet<RuntimeId>, ()> {
    let leases = node_lease::list_leases(session).await.map_err(|_| ())?;
    let now_ms = u64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    let margin_ms =
        u64::try_from(SupervisionTiming::default().margin.as_millis()).unwrap_or(u64::MAX);

    let fresh = leases
        .into_iter()
        .filter(|(_, lease)| {
            now_ms.saturating_sub(lease.seq) <= lease.ttl_ms.saturating_add(margin_ms)
        })
        .map(|(id, _)| id)
        .collect();

    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use cell_protocol::{CapabilityTag, ExecutionCapabilities};

    use super::{ExecRuntimeInfo, runtime_has_tags};

    fn runtime_with_tags(tags: &[&str]) -> ExecRuntimeInfo {
        let tags = tags.iter().map(|t| CapabilityTag::new(*t)).collect();
        ExecRuntimeInfo::new(
            zenoh::config::ZenohId::default(),
            None,
            ExecutionCapabilities::new(tags),
        )
    }

    #[test]
    fn no_required_tags_matches_any_runtime() {
        assert!(runtime_has_tags(&runtime_with_tags(&[]), &[]));
        assert!(runtime_has_tags(&runtime_with_tags(&["linux"]), &[]));
    }

    #[test]
    fn matches_when_runtime_has_all_required_tags() {
        assert!(runtime_has_tags(
            &runtime_with_tags(&["linux", "gpu"]),
            &["linux"]
        ));
    }

    #[test]
    fn does_not_match_when_a_required_tag_is_missing() {
        assert!(!runtime_has_tags(&runtime_with_tags(&["linux"]), &["gpu"]));
    }
}
