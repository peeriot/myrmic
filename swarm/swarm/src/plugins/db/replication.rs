use db_client::replica_v1::Client as ReplicaClient;
use db_commons::models::{ReplicaMessage, Subject};
use zenoh::Session;

#[derive(Clone)]
pub(crate) struct ZenohTransport {
    pub(crate) client: ReplicaClient,
    /// Whether this transport moves data for a locate-visible replica or an
    /// offload drain — a pull by the former delivers rows where reads can
    /// route, a pull by the latter does not.
    role: &'static str,
    /// Reads a version stamped now, for measuring how old pulled data is by the
    /// time it lands here. Boxed rather than holding the store, so the transport
    /// stays free of the store's metadata type parameter.
    now: std::sync::Arc<dyn Fn() -> db_commons::models::Version + Send + Sync>,
}

impl ZenohTransport {
    pub fn new<M: Send + Sync + 'static>(
        session: &Session,
        subject: &Subject,
        store: &db::store::fjall::Store<M>,
        role: &'static str,
    ) -> Self {
        let client = ReplicaClient::new(session, subject.clone())
            .expect("unable to create replication client");

        let store = store.clone();
        ZenohTransport {
            client,
            role,
            now: std::sync::Arc::new(move || store.now()),
        }
    }
}

impl ZenohTransport {
    /// Publishes `msg`, recording it; returns its encoded size.
    async fn send(&self, msg: ReplicaMessage) -> usize {
        let kind = msg.name();

        if let ReplicaMessage::Announce(announce) = &msg {
            super::metrics::record_announce(
                announce.known.len(),
                announce.known.iter().map(|(_, s)| s.heads.len()).sum(),
                announce
                    .known
                    .iter()
                    .filter(|(_, s)| s.baseline.is_some())
                    .count(),
            );
        }

        let bytes = self.client.publish(msg).await;
        super::metrics::record_msg_sent(kind, bytes);
        bytes
    }
}

impl db::replication::ReplicaTransport for ZenohTransport {
    async fn publish(&self, msg: ReplicaMessage) {
        self.send(msg).await;
    }

    async fn publish_announce(
        &self,
        reason: db::replication::AnnounceReason,
        announce: db_commons::models::replication::Announce,
    ) {
        let bytes = self.send(ReplicaMessage::Announce(announce)).await;
        super::metrics::record_announce_reason(self.role, reason.name(), bytes);
    }

    async fn publish_probe(
        &self,
        reason: db::replication::ProbeReason,
        probe: db_commons::models::replication::Probe,
    ) {
        self.send(ReplicaMessage::Probe(probe)).await;
        super::metrics::record_probe_reason(self.role, reason.name());
    }

    fn can_sync(&self) -> bool {
        true
    }

    async fn pull(
        &self,
        target: uhlc::ID,
        req: db_commons::models::replication::sync::PullRequest,
    ) -> Option<db_commons::models::replication::sync::PullResponse> {
        let started = std::time::Instant::now();
        let namespace = req.scope.namespace.clone();
        let response = self.client.sync_pull(target, req).await;

        super::metrics::record_pull(
            self.role,
            &namespace,
            started.elapsed(),
            response.as_ref().map_or(0, |page| page.chunks.len()),
        );

        // How old each row is by the time it gets here. Everything instrumented
        // so far sums to about 7ms per row against a measured 1.6s, so this is
        // the bisection: an age of milliseconds means the data arrives promptly
        // and the wait is the recipient not looking, while an age of seconds
        // means arrival itself is what is slow.
        if let Some(page) = response.as_ref() {
            let now = (self.now)();
            for chunk in &page.chunks {
                let (_epoch, version, _node) = chunk.id;
                super::metrics::record_applied_age(now, version);
            }
        }

        response
    }

    async fn verify(
        &self,
        target: uhlc::ID,
        req: db_commons::models::replication::sync::VerifyRequest,
    ) -> Option<bool> {
        self.client.sync_verify(target, req).await
    }
}
