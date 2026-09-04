use std::sync::Arc;
use std::time::Duration;

use std::borrow::ToOwned;

use cell_protocol::node_tags::LiveTags;
use sorg_common::{
    CapabilityTag, ExecConfig, ExecRuntimeInfo, ExecutionCapabilities, bail, poison_channel,
    set_up_queryable,
};
use tracing::{debug, info, warn};
use zenoh::Session;

use crate::{Result, event_loop::set_up_event_loop, queryables::Queryable};

/// This node's registry entry as its tags stand now.
///
/// The tags are the node's, not this plugin's: the same set decides which data
/// the node replicates. Placement reads them from the registry row rather than
/// asking the runtime, so the row has to be rewritten whenever they change.
pub(crate) fn runtime_info(
    session: &Session,
    name: Option<String>,
    tags: &LiveTags,
) -> ExecRuntimeInfo {
    let tags = tags.get().iter().map(CapabilityTag::new).collect();

    ExecRuntimeInfo::new(session.zid(), name, ExecutionCapabilities::new(tags))
}

/// Spawns a tokio task running the sorg orchestration. The task can be terminated using
/// the sender part of the one-shot channel which is provided to this method
pub async fn spawn(
    session: Session,
    config: ExecConfig,
    tags: LiveTags,
    off_rcv: flume::Receiver<()>,
    ready: Arc<tokio::sync::Notify>,
) -> Result<()> {
    info!("spawning sorg-execution");

    // BLE is served per-cell (each cell owns an in-process backend), so there is
    // no node-global BLE task to start here.

    let name = config.name().map(ToOwned::to_owned);
    let exec_info = runtime_info(&session, name.clone(), &tags);

    // the senders are not actively used, but will poison the other side as soon as we leave the scope
    let (_poison_snd_event_loop, poison_rcv_event_loop) = poison_channel();
    let (client, handle_event_loop) =
        set_up_event_loop(session.clone(), config, poison_rcv_event_loop);

    let (handle_capas, _poison_snd_capas) =
        set_up_queryable(session.clone(), client.handle(), Queryable::Capabilities);
    let (handle_cell_deploy, _poison_snd_cell_deploy) =
        set_up_queryable(session.clone(), client.handle(), Queryable::CellDeploy);
    let (handle_cell_undeploy, _poison_snd_cell_undeploy) =
        set_up_queryable(session.clone(), client.handle(), Queryable::CellUndeploy);

    // wait a bit for evth to set up before signaling readiness
    tokio::time::sleep(Duration::from_millis(100)).await;

    let runtime_id: cell_protocol::RuntimeId = session.zid().into();

    // Release any cell rows this node left behind in a previous incarnation
    // before advertising as available, so a redeploy of the same SRI can't race
    // the first verify-pass sweep and be rejected as already deployed.
    crate::supervision::startup::sweep_previous_incarnation(&session, runtime_id).await;

    register_in_exec_registry(&session, &exec_info).await?;

    let _retagging = tokio::spawn(republish_on_retag(
        session.clone(),
        name.clone(),
        tags.clone(),
    ));

    // Linux execs have no stable device id today; the runtime id stands in.
    let _renewal = crate::supervision::spawn_renewal(
        session.clone(),
        runtime_id,
        runtime_id.to_string(),
        sorg_common::supervision::SupervisionTiming::default(),
        name,
        tags,
    );

    ready.notify_one();
    info!("exec plugin ready");

    tokio::select! {

        exec_result = handle_event_loop => {
            let exec_result = match exec_result{
                Ok(res) => res,
                Err(join_err) => {
                    bail!("the exec event loop has panicked or was canceled: {join_err}");
                }
            };
            match exec_result {
                Ok(()) => bail!("exec event loop terminated without error"),
                Err(err) => {
                        bail!("exec event loop terminated due to error: '{err}'")
                            }
            }
        }

        _ = handle_capas => {
            bail!("capas queryable terminated");
        }

        _ = handle_cell_deploy => {
            bail!("cell deploy queryable terminated");
        }

        _ = handle_cell_undeploy => {
            bail!("cell undeploy queryable terminated");
        }

        _ = off_rcv.recv_async() => {
            debug!("sorg execution received shutdown signal");
        }
    }
    debug!("shutting down sorg execution");
    Ok(())
}

/// Rewrites this node's registry row whenever its tags change, so a retag
/// reaches placement without waiting for a restart. Runs until the session
/// drops.
async fn republish_on_retag(session: Session, name: Option<String>, tags: LiveTags) {
    let mut retagged = tags.subscribe();

    while retagged.changed().await.is_ok() {
        let info = runtime_info(&session, name.clone(), &tags);

        match sorg_common::exec_registry::register_exec(&session, &info).await {
            Ok(()) => info!("re-registered with tags: {}", tags.get().join(", ")),
            // The renewal's heal pass notices the stale row on its slow
            // cadence, so a failure here delays the new tags rather than
            // losing them.
            Err(err) => warn!("unable to re-register after a tag change: {err}"),
        }
    }
}

const REGISTER_TIMEOUT: Duration = Duration::from_secs(2);
const REGISTER_MAX_ATTEMPTS: u32 = 5;

async fn register_in_exec_registry(session: &Session, info: &ExecRuntimeInfo) -> Result<()> {
    tryhard::retry_fn(|| async {
        let result = tokio::time::timeout(
            REGISTER_TIMEOUT,
            sorg_common::exec_registry::register_exec(session, info),
        )
        .await;
        match result {
            Ok(inner) => inner.map_err(crate::Error::from),
            Err(_) => bail!("exec registration timed out"),
        }
    })
    .retries(REGISTER_MAX_ATTEMPTS)
    .fixed_backoff(REGISTER_TIMEOUT)
    .on_retry(|attempt, _, err: &_| {
        let err = err.to_string();
        async move {
            debug!("exec registration failed (attempt {attempt}/{REGISTER_MAX_ATTEMPTS}): {err}");
        }
    })
    .await
    .map_err(|err| {
        tracing::error!(
            "exec registration failed after {REGISTER_MAX_ATTEMPTS} attempts — is a DB plugin running in the swarm?"
        );
        crate::Error::from(sorg_common::custom_err!(
            "exec registration failed after {REGISTER_MAX_ATTEMPTS} attempts — is a DB plugin running in the swarm? last error: {err}"
        ))
    })
}
