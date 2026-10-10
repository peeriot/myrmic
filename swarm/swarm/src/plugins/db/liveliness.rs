//! A db-owned liveliness token per node, and the watch that forgets a peer
//! the moment its token goes.
//!
//! Without it a departed peer is only aged out: `peer_view` keeps vouching for
//! it in every locate reply until `PEER_TTL` (42 s), and for a scope every
//! node holds at the same head the rendezvous draw picks the holder — so a
//! departed node that won the draw keeps every client's locate routed at a
//! queryable that no longer exists, for the whole TTL. Zenoh learns of the
//! departure at once (a closed session, or an expired link lease); this hands
//! that knowledge to the store.

use std::time::Duration;

use zenoh::{liveliness::LivelinessToken, sample::SampleKind};

use db_commons::topics::liveliness;

use super::StoreContext;

/// How often the token is reconciled with whether the store accepts writes.
const AVAILABILITY_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Declares this node's token and forgets each peer whose token is deleted,
/// until the store shuts down. The token is undeclared on the way out so a
/// graceful shutdown signals the departure before the session closes.
///
/// The token is also withdrawn while the store refuses writes, so peers stop
/// vouching for this node at once rather than after `PEER_TTL`.
pub(super) async fn watch(context: StoreContext) {
    let me = context.id();
    let session = context.session.clone();
    let shutdown = context.store.shutdown_token();

    let subscriber = match session
        .liveliness()
        .declare_subscriber(liveliness::format_all())
        .await
    {
        Ok(subscriber) => subscriber,
        Err(err) => {
            tracing::error!("unable to watch db liveliness: {err}");
            return;
        }
    };

    let mut token = None;
    let mut availability = tokio::time::interval(AVAILABILITY_CHECK_INTERVAL);
    availability.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let sample = tokio::select! {
            () = shutdown.cancelled() => break,
            _ = availability.tick() => {
                match (context.store.is_unavailable(), token.take()) {
                    (true, Some(held)) => {
                        tracing::warn!("[{me}] withdrawing the db liveliness token while the store refuses writes");
                        undeclare(held).await;
                    }
                    (false, None) => token = declare(&session, me).await,
                    (_, held) => token = held,
                }
                continue;
            }
            sample = subscriber.recv_async() => match sample {
                Ok(sample) => sample,
                Err(_) => break,
            },
        };

        if !matches!(sample.kind(), SampleKind::Delete) {
            continue;
        }

        match liveliness::parse_node(sample.key_expr().as_str()) {
            Ok(peer) => {
                tracing::debug!("[{me}] peer [{peer}] left; forgetting what it announced");
                context.store.forget_peer(&peer.to_le_bytes());
            }
            Err(err) => tracing::warn!("ignoring a malformed db liveliness keyexpr: {err}"),
        }
    }

    if let Some(token) = token {
        undeclare(token).await;
    }
}

async fn declare(session: &zenoh::Session, me: uhlc::ID) -> Option<LivelinessToken> {
    match session
        .liveliness()
        .declare_token(liveliness::format(me))
        .await
    {
        Ok(token) => Some(token),
        Err(err) => {
            tracing::error!("unable to declare the db liveliness token: {err}");
            None
        }
    }
}

async fn undeclare(token: LivelinessToken) {
    if let Err(err) = token.undeclare().await {
        tracing::debug!("unable to undeclare the db liveliness token: {err}");
    }
}
