use std::str::FromStr;

use sorg_common::{Client as EventLoopClient, custom_err};
use tokio::task::JoinHandle;
use tracing::{Instrument, debug, error, warn};
use zenoh::{Session, config::ZenohId, sample::SampleKind};

use crate::{
    Event, Result,
    topics::{TOPIC_ORCH_LIVELINESS_ALL, TOPIC_ORCH_LIVELINESS_PREFIX},
};

/// Starts the task that turns the orch liveliness tokens present in the swarm, now and later, into
/// the events expressing changes of the known set of orchs.
///
/// A history subscriber delivers the tokens that already exist and all later changes through one
/// ordered stream, so no orch can be missed between the initial snapshot and the live updates.
///
/// The returned handle finishes only if the task gives up, which the caller must treat as fatal.
///
/// # Errors
/// Fails if the liveliness subscriber cannot be declared.
pub(crate) async fn monitor_orch_nodes(
    session: Session,
    el_client: EventLoopClient<Event>,
) -> Result<JoinHandle<()>> {
    let own_id = session.zid();
    let subscriber = session
        .liveliness()
        .declare_subscriber(TOPIC_ORCH_LIVELINESS_ALL)
        .history(true)
        .await
        .map_err(|err| custom_err!("orch failed to subscribe to orch liveliness: {err}"))?;

    let handle = tokio::spawn(
        async move {
            loop {
                let sample = match subscriber.recv_async().await {
                    Ok(sample) => sample,
                    Err(err) => {
                        error!("orch liveliness subscriber failed: {err}");
                        break;
                    }
                };
                let key = sample.key_expr().as_str();
                let Some(zid) = key
                    .strip_prefix(TOPIC_ORCH_LIVELINESS_PREFIX)
                    .and_then(|id| ZenohId::from_str(id).ok())
                else {
                    warn!("ignoring orch liveliness change with unparsable key '{key}'");
                    continue;
                };
                if zid == own_id {
                    continue;
                }
                debug!(
                    "processing {kind:?} of orch with id {zid}",
                    kind = sample.kind()
                );
                let event = match sample.kind() {
                    SampleKind::Put => Event::OrchJoining(zid),
                    SampleKind::Delete => Event::NodeLeaving(zid),
                };
                if el_client.send(event).await.is_err() {
                    error!("event loop is gone, stopping orch membership monitor");
                    break;
                }
            }
        }
        .in_current_span(),
    );

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sorg_common::Client;
    use sorg_tests::test_session;
    use tokio::{sync::mpsc, task::JoinHandle, time::timeout};
    use zenoh::{Session, liveliness::LivelinessToken};

    use super::monitor_orch_nodes;
    use crate::{Event, topics::topic_orch_liveliness};

    const EVENT_TIMEOUT: Duration = Duration::from_secs(2);

    #[tokio::test(flavor = "multi_thread")]
    async fn orch_present_before_monitor_starts_is_reported() {
        let (own, peer) = connected_sessions().await;
        let _own_token = declare_orch_token(&own).await;
        let _peer_token = declare_orch_token(&peer).await;
        let (client, mut rx) = event_client();

        let _monitor = start_monitor(&own, client).await;

        let event = timeout(EVENT_TIMEOUT, rx.recv()).await;
        let Ok(Some(Event::OrchJoining(zid))) = event else {
            panic!("unexpected event: {event:?}");
        };
        assert_eq!(zid, peer.zid());
        // An own-id join would poison the state and kill the event loop.
        assert!(
            timeout(Duration::from_millis(300), rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn orch_token_dropped_is_reported_as_leaving() {
        let (own, peer) = connected_sessions().await;
        let peer_token = declare_orch_token(&peer).await;
        let (client, mut rx) = event_client();

        let _monitor = start_monitor(&own, client).await;
        let event = timeout(EVENT_TIMEOUT, rx.recv()).await;
        let Ok(Some(Event::OrchJoining(zid))) = event else {
            panic!("unexpected event: {event:?}");
        };
        assert_eq!(zid, peer.zid());

        drop(peer_token);

        let event = timeout(EVENT_TIMEOUT, rx.recv()).await;
        let Ok(Some(Event::NodeLeaving(zid))) = event else {
            panic!("unexpected event: {event:?}");
        };
        assert_eq!(zid, peer.zid());
    }

    async fn connected_sessions() -> (Session, Session) {
        let own = test_session().await;
        let peer = test_session().await;
        let peer_zid = peer.zid();
        timeout(Duration::from_secs(5), async {
            while !own.info().peers_zid().await.any(|zid| zid == peer_zid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("sessions did not discover each other");

        (own, peer)
    }

    async fn start_monitor(session: &Session, client: Client<Event>) -> JoinHandle<()> {
        timeout(EVENT_TIMEOUT, monitor_orch_nodes(session.clone(), client))
            .await
            .expect("monitor start timed out")
            .expect("subscriber declared")
    }

    async fn declare_orch_token(session: &Session) -> LivelinessToken {
        session
            .liveliness()
            .declare_token(topic_orch_liveliness(session.zid()))
            .await
            .expect("failed to declare orch token")
    }

    fn event_client() -> (Client<Event>, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel(16);

        (Client::new(tx), rx)
    }
}
