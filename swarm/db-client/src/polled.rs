//! The one place the "wake on db change, with a periodic poll as a backstop"
//! pattern lives. Previously this `Notify` + `db.subscribe` + `interval` +
//! `select!` idiom was copy-pasted across the command loop, the event queue,
//! and several gateway loops.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio::time::Interval;

use crate::v1::{
    Client, Subscription,
    models::{Scope, Subject, Version},
};

/// Watches one or more db tables for changes. Subscribes for prompt wakeups;
/// the periodic poll (driven by the caller's [`Interval`]) is a best-effort
/// backstop, since notifications aren't guaranteed to be delivered — and
/// changes that arrive by replication don't raise a local one at all.
///
/// The caller owns the [`Interval`] so it can decide the cadence and whether it
/// persists across polls (commands) or is created per receive call (events).
pub struct PolledTable {
    notify: Arc<Notify>,
    received: Arc<AtomicU64>,
    latest: Arc<Mutex<HashMap<Scope, Version>>>,
    _subscriptions: Vec<Subscription>,
}

impl PolledTable {
    /// Subscribe to `table` under `subject`. A failed subscription is not fatal:
    /// we log and fall back to the caller's periodic poll.
    pub async fn new(db: &Client, subject: Subject, table: &str) -> Self {
        Self::tables(db, [(subject, table)]).await
    }

    /// Subscribe to several tables at once, funnelling all of them into the
    /// same wakeup. Use when a single loop reconciles state that more than one
    /// table feeds into.
    pub async fn tables<'a, I>(db: &Client, tables: I) -> Self
    where
        I: IntoIterator<Item = (Subject, &'a str)>,
    {
        let notify = Arc::new(Notify::new());
        let received = Arc::new(AtomicU64::new(0));
        let latest = Arc::new(Mutex::new(HashMap::<Scope, Version>::new()));
        let mut subscriptions = Vec::new();

        for (subject, table) in tables {
            let subscription = db
                .subscribe(subject, table, {
                    let notify = notify.clone();
                    let received = received.clone();
                    let latest = latest.clone();
                    move |notification| {
                        received.fetch_add(1, Ordering::Relaxed);
                        let version = notification.version();
                        latest
                            .lock()
                            .expect("latest-version map poisoned")
                            .entry(notification.scope)
                            .and_modify(|seen| *seen = (*seen).max(version))
                            .or_insert(version);
                        notify.notify_one();
                    }
                })
                .await;

            match subscription {
                Ok(sub) => subscriptions.push(sub),
                Err(err) => {
                    tracing::warn!("table subscription failed; relying on periodic poll: {err}");
                }
            }
        }

        Self {
            notify,
            received,
            latest,
            _subscriptions: subscriptions,
        }
    }

    /// The highest version any notification has reported for `scope`.
    ///
    /// A change's event outruns its replication, so a watcher reading on the
    /// wake should route there (see [`Client::read_tx_at`]) rather than trust
    /// whichever copy it would otherwise reach.
    pub fn latest(&self, scope: &Scope) -> Option<Version> {
        self.latest
            .lock()
            .expect("latest-version map poisoned")
            .get(scope)
            .copied()
    }

    /// Notifications delivered to this watcher since it was opened, counted
    /// before [`Notify`] collapses them.
    ///
    /// The distinction matters: `notify_one` holds at most one permit, so a
    /// burst of notifications arriving while the caller is busy yields a single
    /// wakeup. Counting wakeups therefore says nothing about how many pokes
    /// were sent, whereas this against the number of rows actually written says
    /// how many never arrived — table events are published as zenoh pushes,
    /// which are dropped rather than queued when a link congests.
    pub fn received(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }

    /// Wait until there might be work: either a change notification arrives or
    /// the backstop interval ticks.
    ///
    /// Which one it was is worth knowing to a caller measuring itself: a read
    /// that finds nothing after the *backstop* fired is just an idle poll,
    /// while one that finds nothing after a *notification* means the change it
    /// was told about was not visible to the read — a different problem
    /// entirely.
    pub async fn wait(&self, interval: &mut Interval) -> Woke {
        let notify = self.notify.clone();
        tokio::select! {
            () = notify.notified() => Woke::Notified,
            _ = interval.tick() => Woke::Backstop,
        }
    }
}

/// Why [`PolledTable::wait`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Woke {
    /// A change notification arrived.
    Notified,
    /// The periodic backstop fired; nothing said there was work.
    Backstop,
}
