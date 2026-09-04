//! Receiving side for events: a cursored, non-destructive read over a public
//! per-event `events` table.
//!
//! Unlike commands, events are a log, not a queue — they are public and never
//! deleted. Each stream keeps a cursor so it only sees events published after
//! it subscribed, advancing past everything it reads (including undecodable
//! entries, which are skipped rather than dead-lettered).

use std::time::Duration;

use cell_protocol::{EVENTS_TABLE, MailboxEvent, scope_of_event};
use db_client::v1::{Client, models};
use myrmic_common::cells::Event;
use tokio::time::{MissedTickBehavior, interval};

use crate::error::{Error, Result, from_bytes};
use db_client::PolledTable;

/// A cursored view over the events published for a single event name.
pub struct EventStream {
    db: Client,
    event: Event,
    scope: models::Scope,
    cursor: models::Cursor,
    polled: PolledTable,
}

impl EventStream {
    /// Subscribe to `event`, starting the cursor past all events that already
    /// exist (so only future events are delivered).
    pub async fn subscribe(db: Client, event: Event) -> Result<Self> {
        let scope = scope_of_event(event.as_ref());
        let polled =
            PolledTable::new(&db, models::Subject::Scope(scope.clone()), EVENTS_TABLE).await;

        let mut stream = Self {
            db,
            event,
            scope,
            cursor: models::Cursor::Skip(0),
            polled,
        };

        let count = stream.count().await?;
        stream.cursor = models::Cursor::Skip(count);

        Ok(stream)
    }

    /// Block until at least one event is available (or `batch_size` are), then
    /// return them. `poll_interval` is the backstop cadence for missed pokes.
    ///
    /// A failed poll is logged and retried on the same cadence rather than
    /// surfaced immediately — errors here are usually transient routing
    /// failures (a drowned locate, a momentarily empty discovery view), the
    /// cursor only advances past what a poll actually returned, and callers
    /// drive their whole event loop through this method: letting one bad poll
    /// escape killed the fan-in cell for the rest of a benchmark pass (run
    /// 33245323105, loads 400/600/1000). The tolerance is *bounded*, though:
    /// `MAX_CONSECUTIVE_POLL_FAILURES` in a row (with a successful poll
    /// resetting the count) means the session under this stream is gone, not
    /// congested — a dying runtime's stream must fail its caller and stop,
    /// not spam locate/discovery queries into a mesh the next deployment is
    /// converging on (retrying forever here made every third rack provision
    /// fail placement: runs 33246397138/33247056175/33247785686).
    pub async fn receive_batch(
        &mut self,
        poll_interval: Duration,
        batch_size: usize,
    ) -> Result<Vec<MailboxEvent>> {
        /// Consecutive failed polls (each already 3 locate+peek attempts deep,
        /// see `db-client`'s `tb_peek`) before the error is the caller's:
        /// transient starvation under load recovers within one or two polls,
        /// while a torn-down session never stops failing.
        const MAX_CONSECUTIVE_POLL_FAILURES: u32 = 8;

        let mut interval = interval(poll_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut consecutive_failures = 0;

        loop {
            match self.poll(batch_size).await {
                Ok(events) if !events.is_empty() => return Ok(events),
                Ok(_) => consecutive_failures = 0,
                Err(err) => {
                    consecutive_failures += 1;
                    if consecutive_failures >= MAX_CONSECUTIVE_POLL_FAILURES {
                        return Err(err);
                    }
                    tracing::error!("unable to poll events, will retry: {err}");
                }
            }
            let _ = self.polled.wait(&mut interval).await;
        }
    }

    /// Non-blocking: read up to `batch_size` events available right now,
    /// advancing the cursor past them. Undecodable entries are skipped.
    pub async fn poll(&mut self, batch_size: usize) -> Result<Vec<MailboxEvent>> {
        tracing::trace!("polling {} // {}", self.scope, EVENTS_TABLE);

        // One routed round trip (a server-side read snapshot) instead of
        // begin/list/rollback.
        let entities = self
            .db
            .send(models::tb_peek::Request {
                scope: self.scope.clone(),
                table: String::from(EVENTS_TABLE),
                cursor: Some(self.cursor.clone()),
                limit: Some(batch_size),
                order: None,
                count: false,
            })
            .await
            .map_err(|err| Error::comm("event list", err))?
            .map_err(|err| Error::db("event list", err.message))?
            .entities;

        let mut events = Vec::with_capacity(entities.len());
        for (msg_id, payload) in entities {
            // Advancing here also skips past events that fail to decode.
            self.cursor = models::Cursor::After(msg_id);

            match from_bytes::<MailboxEvent>(&payload, "deserialise event") {
                Ok(event) => events.push(event),
                Err(err) => tracing::error!("unable to deserialise event, skipping... {err}"),
            }
        }

        Ok(events)
    }

    /// The number of events currently stored (not the number unseen by this
    /// cursor). Used to position the initial cursor.
    pub async fn count(&self) -> Result<usize> {
        let count = self
            .db
            .send(models::tb_peek::Request {
                scope: self.scope.clone(),
                table: String::from(EVENTS_TABLE),
                cursor: None,
                limit: Some(0),
                order: None,
                count: true,
            })
            .await
            .map_err(|err| Error::comm("event count", err))?
            .map_err(|err| Error::db("event count", err.message))?
            .count
            .unwrap_or(0);

        Ok(count)
    }

    /// The event name this stream is reading.
    pub fn event(&self) -> &Event {
        &self.event
    }
}
