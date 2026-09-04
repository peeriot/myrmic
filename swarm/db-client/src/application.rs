//! Batched transactions.
//!
//! An *application* is one round trip against a transaction: find or continue
//! it, apply a batch of operations in order, then commit or leave it open.
//!
//! Operations whose response carries nothing a caller can want ([`Deferrable`])
//! buffer here in program order and cost nothing when issued. The first
//! operation that returns a value flushes the buffer with itself appended last,
//! and its response is the round trip's response — the tail rule. Whatever the
//! values a caller demands force, one [`Application`] is one transaction and
//! all-or-nothing: best case a single self-committing round trip, worst case no
//! more chatter than sending every operation separately.

use core::time::Duration;

use db_commons::models::*;

use crate::v1::Client;

#[cfg(feature = "nano")]
use alloc::{format, string::String, vec::Vec};

#[derive(Debug)]
pub enum Error {
    /// The mesh could not be reached. The transaction's fate is unknown, so the
    /// application is abandoned to the server's idle timeout.
    Unreachable(String),
    /// The application was refused, or the transaction was rolled back. Nothing
    /// it carried was committed.
    Refused(tx_apply::Error),
}

impl Error {
    pub fn message(&self) -> &str {
        match self {
            Self::Unreachable(message) => message,
            Self::Refused(err) => &err.message,
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreachable(message) => write!(f, "unable to reach the db: {message}"),
            Self::Refused(err) => match err.index {
                Some(index) => write!(f, "op {index} failed: {}", err.message),
                None => write!(f, "{}", err.message),
            },
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// A transaction being built up one operation at a time.
pub struct Application {
    client: Client,
    constraint: tx_begin::Constraint,
    access: tx_begin::Access,
    retention_period: Option<Duration>,
    /// The open transaction, once a flush has placed one.
    tx: Option<TxId>,
    /// Deferred operations, in program order, waiting for something to flush
    /// them.
    pending: Vec<TxOp>,
    /// Set once a flush has failed. The transaction is gone either way — rolled
    /// back server-side, or of unknown fate behind a transport error — so
    /// everything after it fails immediately rather than building on a
    /// transaction that no longer exists.
    poisoned: bool,
}

impl Application {
    /// A write application routed to a holder of `scope`.
    pub fn routed(client: Client, scope: Scope) -> Self {
        Self::new(client, tx_begin::Constraint::Routed(scope))
    }

    pub fn new(client: Client, constraint: tx_begin::Constraint) -> Self {
        Self {
            client,
            constraint,
            access: tx_begin::Access::Write,
            retention_period: None,
            tx: None,
            pending: Vec::new(),
            poisoned: false,
        }
    }

    /// Declares the application read-only, so a fallback landing leaves no
    /// trace on the node that answers it. Only for applications that never
    /// write: a write placed as a read gets no findable holder, and the next
    /// read may not see it.
    pub fn read_only(mut self) -> Self {
        self.access = tx_begin::Access::Read;
        self
    }

    pub fn retain_for(mut self, retention_period: Duration) -> Self {
        self.retention_period = Some(retention_period);
        self
    }

    /// The client this application talks to.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The transaction, if one is open. `None` means nothing has been flushed
    /// yet — there is no transaction to name.
    pub fn tx(&self) -> Option<TxId> {
        self.tx
    }

    /// Whether anything at all has happened: a transaction was opened, or
    /// operations are waiting to be applied.
    pub fn is_empty(&self) -> bool {
        self.tx.is_none() && self.pending.is_empty()
    }

    /// Whether a flush has already failed, taking the transaction with it.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// The transaction, opening one — and applying anything deferred — if
    /// there isn't one yet.
    ///
    /// For callers that have to name the transaction because they still issue
    /// their own per-operation requests. Deferred operations apply first, so
    /// program order holds either way.
    pub async fn tx_id(&mut self) -> Result<TxId> {
        if let Some(id) = self.tx
            && self.pending.is_empty()
        {
            return Ok(id);
        }

        let response = self.flush(None, tx_apply::Finish::KeepOpen).await?;

        response.tx.ok_or_else(|| {
            Error::Refused(tx_apply::Error {
                message: String::from("application left no transaction open"),
                index: None,
            })
        })
    }

    /// Buffers an operation. Free at issue time; it applies at the next flush,
    /// in the order it was deferred.
    ///
    /// Only a [`Deferrable`] operation can be deferred — anything that returns
    /// a value has to be the tail of an application, which is [`apply`].
    ///
    /// Refused once the application is poisoned. The transaction the operation
    /// would join is already gone, so buffering it would tell the caller a
    /// write landed that can never commit.
    ///
    /// [`apply`]: Self::apply
    pub fn defer<T: Deferrable>(&mut self, op: T) -> Result<()> {
        self.defer_op(op.into())
    }

    /// [`defer`](Self::defer) for an operation that has already lost its type.
    ///
    /// The tail rule is the caller's to keep here: an op that returns a value
    /// the caller wanted is silently discarded rather than refused. For callers
    /// that erase ops before they reach an application — a host bridge carrying
    /// them across a channel, say — and classified them on the way in.
    pub fn defer_op(&mut self, op: TxOp) -> Result<()> {
        if self.poisoned {
            return Err(Self::poison());
        }

        self.pending.push(op);

        Ok(())
    }

    /// Applies everything deferred so far with `op` last, and returns `op`'s
    /// response. The transaction is left open for whatever comes next.
    pub async fn apply<T: Operation>(&mut self, op: T) -> Result<T::Response> {
        let last = self.apply_op(op.into()).await?;

        T::Response::try_from(last).map_err(|_| {
            Error::Refused(tx_apply::Error {
                message: format!("application returned the wrong response for {}", T::NAME),
                index: None,
            })
        })
    }

    /// [`apply`](Self::apply) for an operation that has already lost its type,
    /// returning the tail response un-narrowed for the caller to match on.
    ///
    /// [`apply`]: Self::apply
    pub async fn apply_op(&mut self, op: TxOp) -> Result<TxOpResponse> {
        let name = op.name();
        let response = self.flush(Some(op), tx_apply::Finish::KeepOpen).await?;

        response.last.ok_or_else(|| {
            Error::Refused(tx_apply::Error {
                message: format!("application returned no response for {name}"),
                index: None,
            })
        })
    }

    /// [`apply_op`](Self::apply_op) that closes the transaction instead of
    /// leaving it open — one round trip for a whole application whose last act
    /// is the operation it wants the value of.
    ///
    /// What a one-shot routed read is made of: a [`read_only`] application
    /// applying a single op, placed on a holder of the scope it names rather
    /// than wherever some other transaction happens to sit.
    ///
    /// [`read_only`]: Self::read_only
    pub async fn apply_and_commit(&mut self, op: TxOp) -> Result<TxOpResponse> {
        let name = op.name();
        let response = self.flush(Some(op), tx_apply::Finish::Commit).await?;

        response.last.ok_or_else(|| {
            Error::Refused(tx_apply::Error {
                message: format!("application returned no response for {name}"),
                index: None,
            })
        })
    }

    /// Applies whatever is left and commits. One round trip — or none at all,
    /// when nothing was ever deferred and no transaction was opened.
    pub async fn commit(mut self) -> Result<()> {
        if self.poisoned {
            return Err(Self::poison());
        }

        if self.is_empty() {
            return Ok(());
        }

        self.flush(None, tx_apply::Finish::Commit).await?;
        self.tx = None;

        Ok(())
    }

    /// Abandons the application. Costs nothing when nothing was ever flushed;
    /// the buffered operations simply never happened.
    pub async fn rollback(mut self) -> Result<()> {
        // A poisoned application has already been rolled back where it counts.
        if self.poisoned {
            self.pending.clear();
            self.tx = None;
            return Ok(());
        }

        let Some(id) = self.tx.take() else {
            self.pending.clear();
            return Ok(());
        };

        self.pending.clear();

        self.client
            .send(tx_rollback::Request { id })
            .await
            .map_err(|err| Error::Unreachable(format!("{err}")))?
            .map_err(|err| {
                Error::Refused(tx_apply::Error {
                    message: err.message,
                    index: None,
                })
            })?;

        Ok(())
    }

    /// One round trip: the pending buffer, `tail` appended, against the open
    /// transaction or a newly placed one.
    ///
    /// Cancellation-safe in the only sense that matters here: the buffer is
    /// emptied before the request goes out, so the application poisons itself
    /// for the duration of the round trip and un-poisons only with a response
    /// in hand. A future dropped mid-flight — a timeout wrapper does exactly
    /// that — then leaves an application that refuses everything after it,
    /// rather than one that has quietly lost its deferred writes and would
    /// still commit.
    async fn flush(
        &mut self,
        tail: Option<TxOp>,
        finish: tx_apply::Finish,
    ) -> Result<tx_apply::Response> {
        if self.poisoned {
            return Err(Self::poison());
        }

        let mut ops = core::mem::take(&mut self.pending);
        ops.extend(tail);

        let target = match self.tx {
            Some(id) => tx_apply::Target::Existing(id),
            None => tx_apply::Target::New {
                constraint: self.constraint.clone(),
                access: self.access,
                retention_period: self.retention_period,
            },
        };

        self.poisoned = true;
        self.tx = None;

        let sent = self
            .client
            .send(tx_apply::Request {
                target,
                ops,
                finish,
            })
            .await;

        let response = match sent {
            Ok(Ok(response)) => response,
            // Either way the transaction is not ours to use any more: a refusal
            // rolled it back, and a transport error leaves its fate unknown.
            Ok(Err(err)) => return Err(Error::Refused(err)),
            Err(err) => return Err(Error::Unreachable(format!("{err}"))),
        };

        self.poisoned = false;
        self.tx = response.tx;

        Ok(response)
    }

    fn poison() -> Error {
        Error::Refused(tx_apply::Error {
            message: String::from("the transaction was already rolled back by a failed operation"),
            index: None,
        })
    }
}
