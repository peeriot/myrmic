use db::store::{TransactionMode, TransactionOptions};
use zenoh::query::Query;

use super::StoreContext;
use db_commons::models::*;
use db_commons::query::{Handler, ReplyTimestamp, parse_query};

use super::apply::{apply, format_error};

pub async fn handle_query(ctx: StoreContext, query: Query) {
    let Some(req) = parse_query::<DbRequest>(&query) else {
        tracing::error!(
            "Unable to parse incoming query. [dropped {}]",
            query.key_expr()
        );
        return;
    };

    let me = ctx.id();

    tracing::trace!(
        "[{}] received {} (via {})",
        me,
        req.name(),
        query.key_expr().as_str()
    );

    match req {
        DbRequest::Ping(value) => ctx.call(value, query).await,
        DbRequest::Info(value) => ctx.call(value, query).await,
        DbRequest::TxApply(value) => ctx.call(value, query).await,
        DbRequest::TxCommit(value) => ctx.call(value, query).await,
        DbRequest::TxRollback(value) => ctx.call(value, query).await,
        DbRequest::TbPeek(value) => ctx.call(value, query).await,
    }
}

impl ReplyTimestamp for StoreContext {
    fn reply_timestamp(&self) -> Option<zenoh::time::Timestamp> {
        Some(self.session.new_timestamp())
    }
}

impl StoreContext {
    /// The placement contract of a routed write landing on this node, shared
    /// by [`tx_begin`] and [`tx_apply`]: absorb the write as a findable sink
    /// when no replica exists, re-arm a running drain when the client proved
    /// it searched and found nobody better, and refuse (`Err` carries the
    /// message) when a live replica should take the write instead.
    fn place_routed_write(&self, scope: &Scope) -> Result<(), String> {
        if self.store.is_replicating(scope) {
            return Ok(());
        }

        if self.store.is_offloading(scope) {
            let replica_visible = self
                .store
                .peer_view(scope, std::time::Instant::now())
                .iter()
                .any(|peer| matches!(peer.state, locate::HolderState::Replica));

            // Quiesced while a replica lives: the drain's holdings must
            // freeze for it to retire, and the client has somewhere better
            // to land — refuse, and it re-locates. Only with no replica
            // visible does the drain absorb the write, and that acceptance
            // is then honest evidence for its escalation re-arm (under
            // catch-up load a client's locate can simply lose its race,
            // and re-arming on such a landing flaps the drain forever).
            if replica_visible {
                return Err(format!(
                    "scope {scope} is draining here while a live replica exists; re-locate"
                ));
            }

            self.rearm_offload(scope);
        } else {
            self.start_offload(scope.clone(), super::OffloadKind::Sink);
        }

        Ok(())
    }

    /// Commits `tx` and runs the duties a remote commit performs afterwards:
    /// offering up committed scopes this node does not replicate, and
    /// publishing the table events the transaction recorded. Shared by
    /// [`tx_commit`] and [`tx_apply`].
    async fn finish_commit(
        &self,
        mut tx: db::store::fjall::Transaction<super::TxEvents>,
    ) -> anyhow::Result<()> {
        let events = tx.take_metadata();

        // The version the commit lands at; published on events so subscribers can
        // resume a transaction from at least this point.
        let version = tx.timestamp().get_time().as_u64();

        let scopes: Vec<Scope> = tx.touched_scopes().cloned().collect();

        tx.commit()?;

        // Data committed to a scope this node doesn't replicate would strand
        // here; offer it up to the nodes that do. A drain already running for
        // the scope is woken rather than left to its periodic announce — a
        // fresh one announces on its first iteration anyway.
        for scope in scopes {
            if self.store.is_replicating(&scope) {
                continue;
            }

            if self.store.is_offloading(&scope) {
                self.nudge_offload(&scope);
            } else {
                self.start_offload(scope, super::OffloadKind::Hidden);
            }
        }

        if let Some(events) = events {
            self.publish_events(events, version).await;
        }

        Ok(())
    }
}

impl Handler<db_info::Request> for StoreContext {
    type Response = db_info::Response;
    type Error = db_info::Error;

    async fn handle(self, _req: db_info::Request) -> Result<Self::Response, Option<Self::Error>> {
        Ok(db_info::Response {
            id: self.id().to_le_bytes(),
        })
    }
}

impl Handler<ping::Request> for StoreContext {
    type Response = ping::Response;
    type Error = ping::Error;

    async fn handle(self, _req: ping::Request) -> Result<Self::Response, Option<Self::Error>> {
        Ok(ping::Response {})
    }
}

impl Handler<tx_commit::Request> for StoreContext {
    type Response = tx_commit::Response;
    type Error = tx_commit::Error;

    async fn handle(self, req: tx_commit::Request) -> Result<Self::Response, Option<Self::Error>> {
        let tx = self.remove_tx(req.id).ok_or_else(|| {
            tracing::warn!("Unable to find tx");
            None
        })?;

        self.finish_commit(tx).await.map_err(|err| {
            tracing::error!("Failed to commit transaction: {:?}", err);
            Some(tx_commit::Error {
                message: format_error(&err),
            })
        })?;

        Ok(tx_commit::Response {})
    }
}

impl Handler<tx_rollback::Request> for StoreContext {
    type Response = tx_rollback::Response;
    type Error = tx_rollback::Error;

    async fn handle(
        self,
        req: tx_rollback::Request,
    ) -> Result<Self::Response, Option<Self::Error>> {
        let tx = self.remove_tx(req.id).ok_or_else(|| {
            tracing::warn!("Unable to find tx");
            None
        })?;

        tx.rollback();

        Ok(tx_rollback::Response {})
    }
}

/// The write side of the db plugin: place or find the transaction, apply the
/// ops in order, then commit or leave it open.
impl Handler<tx_apply::Request> for StoreContext {
    type Response = tx_apply::Response;
    type Error = tx_apply::Error;

    async fn handle(self, req: tx_apply::Request) -> Result<Self::Response, Option<Self::Error>> {
        let tx_apply::Request {
            target,
            ops,
            finish,
        } = req;

        match target {
            tx_apply::Target::New {
                constraint,
                access,
                retention_period,
            } => {
                self.apply_new(constraint, access, retention_period, ops, finish)
                    .await
            }
            tx_apply::Target::Existing(id) => self.apply_existing(id, ops, finish).await,
        }
    }
}

impl StoreContext {
    /// Applies against a freshly placed transaction — the contract a begin used
    /// to carry on its own, unchanged: reassert a version bound, run the routed
    /// write's custody logic, then apply.
    ///
    /// A self-committing application never enters the registry: the transaction
    /// cannot outlive this handler, so it needs no entry and no idle timeout.
    async fn apply_new(
        self,
        constraint: tx_begin::Constraint,
        access: tx_begin::Access,
        retention_period: Option<core::time::Duration>,
        ops: Vec<TxOp>,
        finish: tx_apply::Finish,
    ) -> Result<tx_apply::Response, Option<tx_apply::Error>> {
        // Resuming from a version observed on a table event: discovery already
        // routed us here as a caught-up holder, but reassert the bound so a
        // stale route (or the feature-less client that skips discovery) can't
        // hand back a transaction that cannot see it.
        if let tx_begin::Constraint::RoutedAt(scope, min_version) = &constraint {
            match self.store.scope_has_version(scope, *min_version) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(Some(tx_apply::Error {
                        message: format!(
                            "node has not applied version {} for the requested scope",
                            min_version
                        ),
                        index: None,
                    }));
                }
                Err(err) => {
                    tracing::error!("Unable to resolve scope version: {}", err);
                    return Err(Some(tx_apply::Error {
                        message: format_error(&err),
                        index: None,
                    }));
                }
            }
        }

        // A routed write landing on a node that doesn't replicate its scope
        // means the client located no live replica and fell back. Hold the
        // scope as a findable sink rather than durably promoting on a single
        // miss: if an existing replica was only transiently unreachable it
        // covers us and we drain away, leaving no trace; only if none turns up
        // do we escalate into custody. A read that located nobody leaves no
        // trace — no data lands, so there is nothing to home.
        //
        // A fallback landing while a drain is already running is the re-arm
        // signal: the client just proved it searched and found nobody better,
        // so an unwinding drain's deference target is evidently unreachable.
        if let tx_begin::Constraint::Routed(scope) = &constraint
            && matches!(access, tx_begin::Access::Write)
            && let Err(message) = self.place_routed_write(scope)
        {
            return Err(Some(tx_apply::Error {
                message,
                index: None,
            }));
        }

        let mut opts = if let Some(retention_period) = retention_period {
            TransactionOptions::retain_for(TransactionMode::ReadWrite, retention_period)
        } else {
            TransactionOptions::write()
        };

        if matches!(finish, tx_apply::Finish::Commit) {
            let mut tx = self.store.begin_local(&opts).map_err(|err| {
                tracing::error!("Unable to start transaction: {}", err);
                Some(tx_apply::Error {
                    message: format_error(&err),
                    index: None,
                })
            })?;

            let last = match apply_all(&mut tx, ops) {
                Ok(last) => last,
                Err(err) => {
                    tx.rollback();
                    return Err(Some(err));
                }
            };

            self.finish_commit(tx).await.map_err(|err| {
                tracing::error!("Failed to commit transaction: {:?}", err);
                Some(tx_apply::Error {
                    message: format_error(&err),
                    index: None,
                })
            })?;

            return Ok(tx_apply::Response { tx: None, last });
        }

        opts.idle_timeout = Some(self.tx_idle_timeout);

        let tx_id = self.new_remote_tx(opts).map_err(|err| {
            tracing::error!("Unable to start transaction: {}", err);
            Some(tx_apply::Error {
                message: format_error(&err),
                index: None,
            })
        })?;

        let last = self.apply_registered(tx_id, ops)?;

        Ok(tx_apply::Response {
            tx: Some(tx_id),
            last,
        })
    }

    /// Applies against a transaction this node already holds. A failed op rolls
    /// the whole transaction back: there are no savepoints, and leaving a
    /// half-applied bundle open would silently break the chain's atomicity.
    async fn apply_existing(
        self,
        id: TxId,
        ops: Vec<TxOp>,
        finish: tx_apply::Finish,
    ) -> Result<tx_apply::Response, Option<tx_apply::Error>> {
        if matches!(finish, tx_apply::Finish::Commit) {
            let mut tx = self.remove_tx(id).ok_or_else(|| {
                tracing::warn!("Unable to find tx");
                None
            })?;

            let last = match apply_all(&mut tx, ops) {
                Ok(last) => last,
                Err(err) => {
                    tx.rollback();
                    return Err(Some(err));
                }
            };

            self.finish_commit(tx).await.map_err(|err| {
                tracing::error!("Failed to commit transaction: {:?}", err);
                Some(tx_apply::Error {
                    message: format_error(&err),
                    index: None,
                })
            })?;

            return Ok(tx_apply::Response { tx: None, last });
        }

        let last = self.apply_registered(id, ops)?;

        Ok(tx_apply::Response { tx: Some(id), last })
    }

    /// Applies to a registered transaction, leaving it open. On failure the
    /// transaction is taken out of the registry and rolled back before the
    /// error goes out, so no client can keep building on a broken chain.
    fn apply_registered(
        &self,
        id: TxId,
        ops: Vec<TxOp>,
    ) -> Result<Option<TxOpResponse>, Option<tx_apply::Error>> {
        // The registry guard has to be released before the entry can be taken
        // out of it, so the failure travels out of this scope.
        let failure = {
            let mut tx = self.find_tx(id).ok_or_else(|| {
                tracing::warn!("Unable to find tx");
                None
            })?;

            match apply_all(&mut tx, ops) {
                Ok(last) => return Ok(last),
                Err(err) => err,
            }
        };

        if let Some(tx) = self.remove_tx(id) {
            tx.rollback();
        }

        Err(Some(failure))
    }
}

/// Applies every op in order, returning the last one's response.
fn apply_all(
    tx: &mut db::store::fjall::Transaction<super::TxEvents>,
    ops: Vec<TxOp>,
) -> Result<Option<TxOpResponse>, tx_apply::Error> {
    let mut last = None;

    for (index, op) in ops.into_iter().enumerate() {
        let name = op.name();

        last = Some(apply(tx, op).map_err(|message| tx_apply::Error {
            message: format!("{name}: {message}"),
            index: Some(u32::try_from(index).unwrap_or(u32::MAX)),
        })?);
    }

    Ok(last)
}

impl Handler<tb_peek::Request> for StoreContext {
    type Response = tb_peek::Response;
    type Error = tb_peek::Error;

    async fn handle(self, req: tb_peek::Request) -> Result<Self::Response, Option<Self::Error>> {
        let tb_peek::Request {
            scope,
            table,
            cursor,
            limit,
            order,
            count,
        } = req;

        // A private snapshot, opened and closed here: dropping it on any exit
        // path is the rollback.
        let mut tx = self
            .store
            .begin_local(&TransactionOptions::read())
            .map_err(|err| {
                tracing::error!("Unable to start transaction: {}", err);
                Some(tb_peek::Error {
                    message: format_error(&err),
                })
            })?;

        let key_scope =
            db::domain::Key::new_scope(&scope.namespace, &scope.database, &scope.schema);

        let entities = tx
            .tb_list(key_scope.table(&table), cursor, limit, order)
            .map_err(|err| {
                Some(tb_peek::Error {
                    message: format_error(&err),
                })
            })?;

        let count = if count {
            Some(tx.tb_count(key_scope.table(&table)).map_err(|err| {
                Some(tb_peek::Error {
                    message: format_error(&err),
                })
            })?)
        } else {
            None
        };

        tx.rollback();

        super::metrics::record_peek_served(
            &scope.namespace,
            self.store.is_replicating(&scope),
            entities.len(),
        );

        Ok(tb_peek::Response { entities, count })
    }
}
