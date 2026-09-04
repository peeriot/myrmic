//! A mock embedded execution runtime used to exercise SORG's embedded
//! deployment path on Linux, without real hardware.
//!
//! The mock registers itself in the exec registry with the capability tags of
//! an embedded [`Platform`] and participates in the DB-mailbox deployment
//! protocol: it consumes [`DeploymentCommand`]s from its deployment table and
//! replies with confirmations according to a configured [`DeployResponseMode`].
//! The artifact transfer and wasm instantiation a real device performs are
//! faked — none of that is observable by SORG.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cell_protocol::{
    CapabilityTag, DEPLOYMENT_RESPONSES_TABLE, DEPLOYMENT_TABLE, DeploymentCommand,
    DeploymentConfirmation, ExecRuntimeInfo, ExecutionCapabilities, RuntimeId, Sri,
    scope_of_deployment,
};
use db_client::v1::Client as DbClient;
use db_client::v1::models::{
    Scope, TxId, tb_delete, tb_insert, tb_list, tx_begin, tx_commit, tx_rollback,
};
use introspection_common::v1::topic_liveliness_own;
use myrmic_tags::Platform;
use zenoh::Session;

/// Interval between deployment-mailbox polls.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How the mock reacts to a received [`DeploymentCommand`].
#[derive(Debug, Clone)]
pub enum DeployResponseMode {
    /// Reply with a successful confirmation (`failure: None`).
    ConfirmSuccess,
    /// Reply with a failed confirmation carrying the given message.
    ConfirmFailure(String),
    /// Consume and delete the command but never reply, so the orchestrator
    /// times out waiting for a confirmation.
    Silent,
}

/// A mock embedded execution runtime running on its own zenoh session.
pub struct MockEmbeddedExec {
    id: RuntimeId,
    received: Arc<Mutex<Vec<DeploymentCommand>>>,
    session: Session,
    poll_task: tokio::task::JoinHandle<()>,
    _liveliness_token: zenoh::liveliness::LivelinessToken,
}

impl MockEmbeddedExec {
    /// Spawns a mock embedded exec runtime, registering it in the exec registry
    /// with the capability tags of `target` and starting the background loop
    /// that consumes deployment commands and responds per `mode`.
    pub async fn spawn(target: Platform, mode: DeployResponseMode) -> Self {
        Self::spawn_with_config(target, mode, zenoh::Config::default()).await
    }

    /// Like [`Self::spawn`], but with an explicit zenoh configuration (e.g. to
    /// set a fixed `ZenohId` or a short transport lease).
    pub async fn spawn_with_config(
        target: Platform,
        mode: DeployResponseMode,
        mut config: zenoh::Config,
    ) -> Self {
        const MAX_ATTEMPTS: u32 = 5;
        const RETRY_DELAY: Duration = Duration::from_secs(2);

        // Join this test process's private multicast group so the mock reaches
        // the swarm started via `swarm_config!`, not a foreign process's swarm —
        // while keeping the rest of the caller's config (e.g. a fixed ZenohId).
        crate::scope_test_multicast(&mut config);
        let session = zenoh::open(config)
            .await
            .expect("mock embedded exec failed to open a zenoh session");
        let id = RuntimeId::from(session.zid());

        let tags = target
            .get_tags()
            .into_iter()
            .map(CapabilityTag::new)
            .collect();
        let info = ExecRuntimeInfo::new(
            session.zid(),
            Some("mock-embedded-exec".to_owned()),
            ExecutionCapabilities::new(tags),
        );
        let mut last_err = None;
        for attempt in 1..=MAX_ATTEMPTS {
            match sorg_common::exec_registry::register_exec(&session, &info).await {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(err) => {
                    tracing::debug!(
                        "mock-embedded: exec registration failed (attempt {attempt}/{MAX_ATTEMPTS}): {err}"
                    );
                    last_err = Some(err);
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
        if let Some(err) = last_err {
            panic!("mock embedded exec failed to register after {MAX_ATTEMPTS} attempts: {err}");
        }

        // Every live node leases (placement drops lease-less execs). One
        // long-ttl lease at spawn stands in for a renewal loop; no test
        // outlives it.
        let seq = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        let lease = cell_protocol::NodeLease {
            device_id: id.to_string(),
            seq,
            ttl_ms: 3_600_000,
        };
        sorg_common::node_lease::renew_lease(&session, id, &lease, Duration::from_hours(2))
            .await
            .expect("mock embedded exec failed to write its node lease");

        // On real hardware, zenoh-nano announces liveliness by sending a raw
        // DeclareSubscriber on the @/liveliness/<zid> key expression — it has
        // no liveliness API. We use declare_token() here because zenoh-rs
        // suppresses regular declare_subscriber() calls whose key expression
        // starts with @/liveliness (they never reach the wire). declare_token()
        // produces the same DeclareSubscriber wire primitive that zenoh-nano
        // will send.
        let liveliness_topic =
            topic_liveliness_own(session.zid()).expect("failed to format liveliness topic");
        let liveliness_token = session
            .liveliness()
            .declare_token(&liveliness_topic)
            .await
            .expect("mock embedded exec failed to declare liveliness token");

        let received = Arc::new(Mutex::new(Vec::new()));
        let poll_task = tokio::spawn(run_poll_loop(
            DbClient::new(&session),
            scope_of_deployment(id),
            mode,
            Arc::clone(&received),
        ));

        Self {
            id,
            received,
            session,
            poll_task,
            _liveliness_token: liveliness_token,
        }
    }

    /// The runtime id the mock registered under, used to address its deployment
    /// scope via [`cell_protocol::scope_of_deployment`].
    #[must_use]
    pub fn id(&self) -> RuntimeId {
        self.id
    }

    /// The deployment commands the mock has consumed so far, in arrival order.
    #[must_use]
    pub fn received_commands(&self) -> Vec<DeploymentCommand> {
        self.received
            .lock()
            .expect("mock embedded exec received-commands mutex poisoned")
            .clone()
    }

    /// Kills the mock by dropping its zenoh session, simulating an embedded
    /// node crash. Returns the deployment commands received before death.
    pub fn kill(self) -> Vec<DeploymentCommand> {
        self.poll_task.abort();
        let commands = self
            .received
            .lock()
            .expect("mock embedded exec received-commands mutex poisoned")
            .clone();
        drop(self.session);
        commands
    }
}

/// Polls the deployment mailbox forever, recording each consumed command and
/// replying per `mode`.
async fn run_poll_loop(
    db: DbClient,
    scope: Scope,
    mode: DeployResponseMode,
    received: Arc<Mutex<Vec<DeploymentCommand>>>,
) {
    loop {
        match consume_one(&db, &scope).await {
            Some(command) => {
                received
                    .lock()
                    .expect("mock embedded exec received-commands mutex poisoned")
                    .push(command.clone());
                match command {
                    DeploymentCommand::Deploy { sri, .. } => {
                        respond(&db, &scope, &mode, sri).await;
                    }
                    DeploymentCommand::Delete { sri } => {
                        write_confirmation(&db, &scope, &DeploymentConfirmation::Deleted { sri })
                            .await;
                    }
                }
            }
            None => tokio::time::sleep(POLL_INTERVAL).await,
        }
    }
}

/// Writes the confirmation dictated by `mode`, or nothing in [`DeployResponseMode::Silent`].
async fn respond(db: &DbClient, scope: &Scope, mode: &DeployResponseMode, sri: Sri) {
    let confirmation = match mode {
        DeployResponseMode::ConfirmSuccess => {
            DeploymentConfirmation::Deployed { failure: None, sri }
        }
        DeployResponseMode::ConfirmFailure(msg) => DeploymentConfirmation::Deployed {
            failure: Some(msg.clone()),
            sri,
        },
        DeployResponseMode::Silent => return,
    };
    write_confirmation(db, scope, &confirmation).await;
}

/// Reads and removes a single [`DeploymentCommand`] from the deployment table
/// within one transaction, committing on success. Returns `None` if the table
/// is empty or any step fails.
async fn consume_one(db: &DbClient, scope: &Scope) -> Option<DeploymentCommand> {
    let tx_id = match db.send(tx_begin::Request::routed(scope.clone())).await {
        Ok(Ok(response)) => response.id,
        Ok(Err(err)) => {
            tracing::warn!("mock-embedded: tx_begin rejected: {}", err.message);
            return None;
        }
        Err(err) => {
            tracing::warn!("mock-embedded: tx_begin transport error: {err}");
            return None;
        }
    };

    let entities = match db
        .send(tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope: scope.clone(),
                table: DEPLOYMENT_TABLE.to_owned(),
                cursor: None,
                limit: Some(1),
                order: None,
            },
        })
        .await
    {
        Ok(Ok(response)) => response.entities,
        Ok(Err(err)) => {
            tracing::warn!("mock-embedded: list deployments rejected: {}", err.message);
            rollback(db, tx_id).await;
            return None;
        }
        Err(err) => {
            tracing::warn!("mock-embedded: list deployments transport error: {err}");
            rollback(db, tx_id).await;
            return None;
        }
    };

    let Some((eid, value)) = entities.into_iter().next() else {
        rollback(db, tx_id).await;
        return None;
    };

    match db
        .send(tb_delete::Request {
            id: tx_id,
            op: tb_delete::Op {
                scope: scope.clone(),
                table: DEPLOYMENT_TABLE.to_owned(),
                eid,
            },
        })
        .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => {
            tracing::warn!("mock-embedded: delete deployment rejected: {}", err.message);
            rollback(db, tx_id).await;
            return None;
        }
        Err(err) => {
            tracing::warn!("mock-embedded: delete deployment transport error: {err}");
            rollback(db, tx_id).await;
            return None;
        }
    }

    let command = match postcard::from_bytes::<DeploymentCommand>(&value) {
        Ok(command) => command,
        Err(err) => {
            tracing::error!("mock-embedded: malformed DeploymentCommand: {err}");
            rollback(db, tx_id).await;
            return None;
        }
    };

    match db.send(tx_commit::Request { id: tx_id }).await {
        Ok(Ok(_)) => Some(command),
        Ok(Err(err)) => {
            tracing::warn!("mock-embedded: commit rejected: {}", err.message);
            None
        }
        Err(err) => {
            tracing::warn!("mock-embedded: commit transport error: {err}");
            None
        }
    }
}

/// Inserts a [`DeploymentConfirmation`] into the responses table.
async fn write_confirmation(db: &DbClient, scope: &Scope, confirmation: &DeploymentConfirmation) {
    let value = match postcard::to_allocvec(confirmation) {
        Ok(value) => value,
        Err(err) => {
            tracing::error!("mock-embedded: failed to serialize confirmation: {err}");
            return;
        }
    };

    let tx_id = match db.send(tx_begin::Request::routed(scope.clone())).await {
        Ok(Ok(response)) => response.id,
        Ok(Err(err)) => {
            tracing::warn!(
                "mock-embedded: tx_begin (confirm) rejected: {}",
                err.message
            );
            return;
        }
        Err(err) => {
            tracing::warn!("mock-embedded: tx_begin (confirm) transport error: {err}");
            return;
        }
    };

    match db
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: scope.clone(),
                table: DEPLOYMENT_RESPONSES_TABLE.to_owned(),
                eid: None,
                value,
            },
        })
        .await
    {
        Ok(Ok(_)) => {
            let _ = db.send(tx_commit::Request { id: tx_id }).await;
        }
        Ok(Err(err)) => {
            tracing::warn!(
                "mock-embedded: insert confirmation rejected: {}",
                err.message
            );
            rollback(db, tx_id).await;
        }
        Err(err) => {
            tracing::warn!("mock-embedded: insert confirmation transport error: {err}");
            rollback(db, tx_id).await;
        }
    }
}

/// Best-effort rollback of an open transaction.
async fn rollback(db: &DbClient, tx_id: TxId) {
    let _ = db.send(tx_rollback::Request { id: tx_id }).await;
}
