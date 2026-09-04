pub use db_commons::models;

use crate::Session;
use db_commons::models::*;
use zenoh_result::ZResult;

#[cfg(feature = "nano")]
use alloc::string::String;

mod impls;
#[cfg(any(feature = "replica", feature = "nano"))]
mod select;

#[derive(Debug, Clone)]
pub struct Client {
    session: Session,
    broadcast: String,
}

#[cfg(feature = "zenoh")]
pub struct Subscription {
    _sub: zenoh::pubsub::Subscriber<()>,
}

/// A [`Client::subscribe`] subscription. Events are pulled via [`Self::recv`].
#[cfg(feature = "nano")]
pub struct Subscription {
    sub: zenoh_nano::ops::subscribe::Subscriber<'static, String>,
}

#[cfg(feature = "nano")]
impl Subscription {
    /// Awaits the next committed table event matching the subscription.
    /// Samples that fail to decode are skipped.
    pub async fn recv(&mut self) -> ZResult<events::Notification> {
        loop {
            let (key, payload) = self
                .sub
                .receive_keyed()
                .await
                .map_err(|err| zenoh_result::zerror!("unable to receive event: {}", err))?;

            let Ok(event) = db_commons::topics::events::parse_event(&key) else {
                crate::log::warn!("dropping event with a malformed keyexpr [{}]", key);
                continue;
            };

            let (namespace, database, schema, table) = event;

            let event = match crate::decode_zbuf::<events::TableEvent>(&payload) {
                Ok(event) => event,
                Err(err) => {
                    crate::log::warn!("dropping malformed event payload: {}", err);
                    continue;
                }
            };

            return Ok(events::Notification {
                scope: Scope::new(namespace, database, schema),
                table: table.into(),
                event,
            });
        }
    }
}

impl Client {
    pub fn new(session: &Session) -> Self {
        let query = db_commons::topics::format_query("*");

        Self {
            // The `Session` is a type definition depending on different feature flags. One of the
            // session types does implement `Copy` while the other only implements `Clone`. To not
            // get a warning for the `Copy` one, we added this exception.
            #[allow(clippy::clone_on_copy)]
            session: session.clone(),
            broadcast: query,
        }
    }

    /// The id of the zenoh runtime backing this client's session. Stable for the
    /// lifetime of the process, unlike a pid, which can be reused after restart.
    #[cfg(feature = "zenoh")]
    pub fn zid(&self) -> zenoh::config::ZenohId {
        self.session.zid()
    }

    /// The db layer makes use of a targeted query system.
    ///
    /// If the target id is provided, then that id is the sole target of the message.
    /// This is used for things that that id has already claimed, ie an ongoing transaction.
    /// Otherwise, they're questions that require acknowledgement from the network.
    /// This is typically handled by the instances in question, and typically only one response should be sent back.
    /// however, it's entirely possible that we receive multiple responses.
    ///
    /// (either because of network splits, or because of a malicious node, or even the same response multiple times, etc.)
    fn format_query(&self, id: Option<NodeId>) -> ZResult<String> {
        if let Some(id) = id {
            let target = uhlc::ID::try_from(&id)
                .map_err(|e| zenoh_result::zerror!("invalid node id: {e}"))?;
            Ok(db_commons::topics::format_query(target))
        } else {
            Ok(self.broadcast.clone())
        }
    }

    async fn direct<R, T, E>(&self, id: NodeId, req: &R) -> ZResult<Result<T, E>>
    where
        R: serde::Serialize + ?Sized,
        T: serde::de::DeserializeOwned,
        E: serde::de::DeserializeOwned,
    {
        let query = self.format_query(Some(id))?;

        crate::direct(&self.session, &query, req).await
    }

    async fn broadcast<'a, R, T, E>(
        &'a self,
        req: &R,
    ) -> ZResult<impl futures::Stream<Item = Result<T, E>> + 'a>
    where
        R: serde::Serialize + ?Sized + 'a,
        T: serde::de::DeserializeOwned + 'a,
        E: serde::de::DeserializeOwned + 'a,
    {
        let query = self.format_query(None)?;

        crate::broadcast(&self.session, &query, req).await
    }

    pub async fn send<T>(&self, req: T) -> ZResult<Result<T::Response, T::Error>>
    where
        T: impls::Request,
    {
        impls::Request::send(req, self).await
    }

    pub async fn ping(&self) -> ZResult<Result<ping::Response, ping::Error>> {
        self.send(ping::Request {}).await
    }

    /// Calls `func` for every committed table event matching `subject` and `table`.
    ///
    /// Events are notifications and shouldn't be treated as correctness.
    /// As in, you _might_ miss an event, so you should still have a timeout, just on the off-chance.
    /// Treat them as pokes.
    #[cfg(feature = "zenoh")]
    pub async fn subscribe<F>(
        &self,
        subject: Subject,
        table: &str,
        func: F,
    ) -> ZResult<Subscription>
    where
        F: Fn(events::Notification) + Send + Sync + 'static,
    {
        let (namespace, database, schema) = subject.as_keyexprs();
        let ke = db_commons::topics::events::format_event(namespace, database, schema, table);

        let sub = self
            .session
            .declare_subscriber(ke)
            .callback(move |sample| {
                if let Some(notification) = parse_notification(&sample) {
                    func(notification);
                }
            })
            .await?;

        Ok(Subscription { _sub: sub })
    }

    /// Subscribes to committed table events matching `subject` and `table`.
    ///
    /// Pull-based: drive the returned [`Subscription`] with [`Subscription::recv`]
    /// from your own task. The same poke semantics as the zenoh variant apply.
    #[cfg(feature = "nano")]
    pub async fn subscribe(&self, subject: Subject, table: &str) -> ZResult<Subscription> {
        let (namespace, database, schema) = subject.as_keyexprs();
        let ke = db_commons::topics::events::format_event(namespace, database, schema, table);

        let sub = zenoh_nano::ops::subscribe::Subscriber::declare(self.session, ke)
            .await
            .map_err(|err| zenoh_result::zerror!("unable to declare subscriber: {}", err))?;

        Ok(Subscription { sub })
    }

    /// Begins a read transaction routed to a node that holds `scope`.
    ///
    /// Prefer this over [`read_tx`](Self::read_tx) wherever the scope is known.
    /// An unrouted transaction is anchored to whichever node the fallback
    /// picked, which need not hold the data the transaction goes on to read.
    /// That is not merely suboptimal for a scope replicated to one designated
    /// node rather than every node (telemetry's `tele/telemetry`): an unrouted
    /// read can land on a node that never received the data at all.
    pub async fn read_tx_in<F, R>(&self, scope: Scope, func: F) -> ZResult<R>
    where
        F: for<'a> AsyncFnOnce(&'a Self, TxId) -> ZResult<R>,
    {
        self.read_tx_constrained(tx_begin::Constraint::Routed(scope), func)
            .await
    }

    /// Begins an unrouted read transaction. Use [`read_tx_in`](Self::read_tx_in)
    /// instead when the scope is known at this point.
    pub async fn read_tx<F, R>(&self, func: F) -> ZResult<R>
    where
        F: for<'a> AsyncFnOnce(&'a Self, TxId) -> ZResult<R>,
    {
        self.read_tx_constrained(tx_begin::Constraint::Ignore, func)
            .await
    }

    async fn read_tx_constrained<F, R>(
        &self,
        constraint: tx_begin::Constraint,
        func: F,
    ) -> ZResult<R>
    where
        F: for<'a> AsyncFnOnce(&'a Self, TxId) -> ZResult<R>,
    {
        let response = self
            .send(tx_begin::Request {
                constraint,
                access: tx_begin::Access::Read,
                ..Default::default()
            })
            .await?
            .map_err(|err| zenoh_result::zerror!("unable to start tx: {}", err.message))?;

        let tx_id = response.id;

        match func(self, tx_id).await {
            Ok(value) => {
                // `value` is only real if the transaction closes, so a failed commit
                // has to become the caller's error rather than a log line.
                self.send(tx_commit::Request { id: tx_id })
                    .await
                    .map_err(|err| zenoh_result::zerror!("unable to communicate with db: {}", err))?
                    .map_err(|err| {
                        zenoh_result::zerror!("unable to commit read transaction: {}", err.message)
                    })?;

                Ok(value)
            }
            Err(err) => {
                // Best-effort: `err` is what the caller needs to see, and replacing it
                // with a rollback failure would lose the cause. Log and leave the
                // transaction to the server's retention timeout.
                match self.send(tx_rollback::Request { id: tx_id }).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        crate::log::error!("unable to rollback read transaction: {}", e.message);
                    }
                    Err(e) => crate::log::error!("unable to communicate with db: {}", e),
                }

                Err(err)
            }
        }
    }

    /// Begins a write transaction routed to a node that holds `scope`.
    ///
    /// Prefer this over [`write_tx`](Self::write_tx) wherever the scope is
    /// known — see [`read_tx_in`](Self::read_tx_in). For a scope replicated to
    /// one designated node the stakes are higher than a missed read: a write
    /// landing on a node that does not (and will not) hold the scope contends
    /// with whichever node replication actually converges on, which is the
    /// "writes contend forever with no consensus" failure
    /// `configure_telemetry_replication` exists to prevent — and that fix only
    /// holds if the writes are routed to the node it designates.
    pub fn write_tx_in<F, R>(&self, scope: Scope, func: F) -> impl Future<Output = ZResult<R>>
    where
        F: for<'a> AsyncFnOnce(&'a Self, TxId) -> ZResult<R>,
    {
        self.write_tx_constrained(tx_begin::Constraint::Routed(scope), None, func)
    }

    /// Begins an unrouted write transaction. Use
    /// [`write_tx_in`](Self::write_tx_in) instead when the scope is known.
    pub fn write_tx<F, R>(&self, func: F) -> impl Future<Output = ZResult<R>>
    where
        F: for<'a> AsyncFnOnce(&'a Self, TxId) -> ZResult<R>,
    {
        self.write_tx_constrained(tx_begin::Constraint::Ignore, None, func)
    }

    pub fn write_tx_with_retention<F, R>(
        &self,
        retention_period: Option<core::time::Duration>,
        func: F,
    ) -> impl Future<Output = ZResult<R>>
    where
        F: for<'a> AsyncFnOnce(&'a Self, TxId) -> ZResult<R>,
    {
        self.write_tx_constrained(tx_begin::Constraint::Ignore, retention_period, func)
    }

    /// [`write_tx_with_retention`](Self::write_tx_with_retention), routed to a
    /// node that holds `scope`.
    pub fn write_tx_in_with_retention<F, R>(
        &self,
        scope: Scope,
        retention_period: Option<core::time::Duration>,
        func: F,
    ) -> impl Future<Output = ZResult<R>>
    where
        F: for<'a> AsyncFnOnce(&'a Self, TxId) -> ZResult<R>,
    {
        self.write_tx_constrained(tx_begin::Constraint::Routed(scope), retention_period, func)
    }

    async fn write_tx_constrained<F, R>(
        &self,
        constraint: tx_begin::Constraint,
        retention_period: Option<core::time::Duration>,
        func: F,
    ) -> ZResult<R>
    where
        F: for<'a> AsyncFnOnce(&'a Self, TxId) -> ZResult<R>,
    {
        let response = self
            .send(tx_begin::Request {
                constraint,
                retention_period,
                access: tx_begin::Access::Write,
            })
            .await?
            .map_err(|err| zenoh_result::zerror!("unable to start tx: {}", err.message))?;

        let tx_id = response.id;

        match func(self, tx_id).await {
            Ok(value) => {
                // Nothing the caller wrote is durable until the commit lands, so a
                // failed commit has to become the caller's error.
                self.send(tx_commit::Request { id: tx_id })
                    .await
                    .map_err(|err| zenoh_result::zerror!("unable to communicate with db: {}", err))?
                    .map_err(|err| {
                        zenoh_result::zerror!("unable to commit write transaction: {}", err.message)
                    })?;

                Ok(value)
            }
            Err(err) => {
                // Best-effort: `err` is what the caller needs to see, and replacing it
                // with a rollback failure would lose the cause. Log and leave the
                // transaction to the server's retention timeout.
                match self.send(tx_rollback::Request { id: tx_id }).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        crate::log::error!("unable to rollback write transaction: {}", e.message);
                    }
                    Err(e) => crate::log::error!("unable to communicate with db: {}", e),
                }

                Err(err)
            }
        }
    }
}

#[cfg(feature = "zenoh")]
fn parse_notification(sample: &zenoh::sample::Sample) -> Option<events::Notification> {
    let ke = sample.key_expr().as_str();

    let (namespace, database, schema, table) = match db_commons::topics::events::parse_event(ke) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::error!("unable to parse event keyexpr [{}]: {}", ke, err);
            return None;
        }
    };

    let event = db_commons::query::parse_sample::<events::TableEvent>(sample)?;

    Some(events::Notification {
        scope: Scope::new(namespace, database, schema),
        table: table.into(),
        event,
    })
}
