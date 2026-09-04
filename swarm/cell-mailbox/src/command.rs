//! Receiving side for commands: a queue over a cell's private `messages` table.
//!
//! A batch is read without removing anything, and each command is handed out
//! with a [`CommandReceipt`]. Removing it is the reader's job: a cell folds the
//! removal into the transaction of the work the command triggered, so a command
//! whose handling failed stays in the mailbox and is read again on a later batch
//! (at-least-once delivery). Readers that run no transaction of their own — the
//! bridges, gateway sessions — remove it immediately instead.
//!
//! Nothing is skipped: the stream always reads from the head of the table, so a
//! command that keeps failing keeps coming back. Only an *undecodable* message
//! is removed without being handled, into the dead-letter table.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cell_protocol::{
    DEADLETTER_TABLE, DeadLetter, DeadLetterType, MESSAGES_TABLE, MailboxCommand, Sri,
    scope_of_cell,
};
use db_client::application::Application;
use db_client::v1::{Client, models};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge};
use tokio::sync::mpsc;
use tokio::time::{Interval, MissedTickBehavior, interval};

use crate::error::{Error, Result, from_bytes, to_bytes};
use db_client::{PolledTable, Woke};

/// A stream of commands for a single cell. Pull one at a time with
/// [`next`](Self::next); reads happen a batch at a time, so a command that
/// fails does not hold up the rest of the batch behind it.
pub struct CommandStream {
    db: Client,
    scope: models::Scope,
    polled: PolledTable,
    interval: Interval,
    batch_size: usize,
    queued: VecDeque<IncomingCommand>,
    /// Which ids a read must not hand out again — see [`Dispatched`].
    dispatched: Dispatched,
    /// Outcomes reported by the receipts handed out, drained at the top of
    /// every read.
    settled: mpsc::UnboundedReceiver<(models::Id, Settlement)>,
    settle: mpsc::UnboundedSender<(models::Id, Settlement)>,
    /// Reads since the queue-depth gauge was last sampled — see [`Self::read_head`].
    reads_since_count: u32,
    /// Why the last wait returned, so an empty read can be told apart from an
    /// idle backstop tick.
    woke: Woke,
    /// Whether the next read is the first since waking. Only that read tests
    /// whether the wakeup's row was visible; the follow-up reads that drain a
    /// burst are unprompted, and the last of them is *expected* to come back
    /// empty — that is how the loop learns the burst ended. Counting those
    /// pins the empty share near 50% at low load whatever the mesh is doing,
    /// which is exactly what the earlier counters measured.
    prompted: bool,
    /// The watcher's notification count as of the last read, so each read can
    /// report only what arrived since.
    notified_seen: u64,
    /// The ids the previous read queued, so a read that turns up nothing the
    /// last one didn't can be told from one that found new work — see
    /// [`Batch::Repeated`].
    last_batch: HashSet<models::Id>,
    metrics: Arc<CommandMetrics>,
}

/// Reads between queue-depth samples: the count behind the `cell_mailbox_size`
/// gauge is a full table scan, and at saturation the backlog is exactly what
/// makes the scan expensive — paying it on every read turns a deep mailbox
/// into extra load on the node already deepest in backlog. One sample every
/// N reads keeps the gauge live at a bounded cost.
const COUNT_SAMPLE_EVERY: u32 = 16;

/// How long a dispatched command is left alone before it's treated as
/// abandoned and made eligible for redelivery. Only reached when a settlement
/// never arrives — the caller died mid-handling, or dropped the receipt — so it
/// is the backstop for at-least-once, not the normal path.
const OUTSTANDING_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a consumed id stays suppressed. Must comfortably exceed the
/// staleness of any view a peek can be answered from; measured worst case on
/// the rack is ~1.8s. If a delete somehow never landed, the row becomes
/// eligible again when this expires, so at-least-once survives even here.
const CONSUMED_MEMORY: Duration = Duration::from_secs(30);

/// Consumed ids held at once, whatever [`CONSUMED_MEMORY`] says. Bounds the
/// memory a cell taking sustained traffic can tie up in tombstones.
const CONSUMED_CAPACITY: usize = 8192;

/// Extra windows one read walks past when a whole window is suppressed.
///
/// The head of the mailbox can be nothing but rows this reader has already
/// dispatched or consumed — a batch whose deletes the replica answering the
/// peek has not applied yet. Stopping at the first window then parks the loop
/// while unsuppressed commands sit just beyond it, pinning throughput to
/// `batch_size` per delete-visibility interval. Bounded so a deep tombstone
/// backlog costs a fixed number of round trips per read rather than a walk of
/// the whole table.
const SUPPRESSED_WINDOWS: usize = 4;

/// What became of a command the stream handed out. Reported by the holder of
/// its [`CommandReceipt`]; the stream cannot tell the two apart by reading,
/// because a rolled-back command and one whose delete has not yet reached the
/// node answering the next peek look identical from there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Settlement {
    /// Its delete committed.
    Consumed,
    /// Its transaction rolled back, or the commit failed — the command is
    /// still in the mailbox and should go round again at the next read.
    Retry,
}

/// The ids a read must not hand out again, and what makes each stop counting.
///
/// The mailbox read is non-destructive, so a command being worked sits at the
/// head exactly like one waiting for the first time. A read's window says
/// nothing about either: peeks are answered by whichever replica locate picks,
/// so a row can be missing from one view and back in the next, and a lost
/// locate returns an empty view that carries no information at all. Only a
/// reported [`Settlement`] retires an id, with two timeouts as backstops.
#[derive(Debug, Default)]
struct Dispatched {
    /// Handed out, outcome not yet reported. Cleared by a settlement or, if
    /// one never arrives (the holder died mid-handling), by
    /// [`OUTSTANDING_TIMEOUT`] — which is what preserves at-least-once.
    outstanding: HashMap<models::Id, Instant>,
    /// Deletes known to have committed, oldest first, held so a view that
    /// predates the delete cannot resurrect the command. Without this,
    /// `bridge.central` ran 14% of its commands twice or more at rack
    /// mid-load, which was the whole 2s p99.
    consumed: VecDeque<(models::Id, Instant)>,
    consumed_ids: HashSet<models::Id>,
}

impl Dispatched {
    /// Whether `id` is being suppressed — either still out with someone, or
    /// consumed recently enough that a stale view could still be showing it.
    fn suppresses(&self, id: &models::Id, now: Instant) -> bool {
        if self.consumed_ids.contains(id) {
            return true;
        }

        self.outstanding
            .get(id)
            .is_some_and(|at| now.duration_since(*at) < OUTSTANDING_TIMEOUT)
    }

    fn dispatch(&mut self, id: models::Id, now: Instant) {
        self.outstanding.insert(id, now);
    }

    fn settle(&mut self, id: &models::Id, settlement: Settlement, now: Instant) {
        self.outstanding.remove(id);

        if settlement == Settlement::Consumed && self.consumed_ids.insert(id.clone()) {
            self.consumed.push_back((id.clone(), now));
        }
    }

    /// Forgets consumed ids old enough that no view can still carry them, and
    /// any beyond [`CONSUMED_CAPACITY`]; drops outstanding ids past the
    /// abandonment timeout.
    ///
    /// Only a settlement removes an outstanding entry, so without this an
    /// abandoned receipt — a cell task killed mid-turn, a command deleted by
    /// something else and so never read back — leaks its entry for the life of
    /// the cell. It stops suppressing at [`OUTSTANDING_TIMEOUT`] either way, so
    /// dropping it there costs nothing and bounds the map at what one
    /// abandonment window's traffic can put in it. Not capped by count as well:
    /// evicting an id still being worked would redeliver a live command, which
    /// is worse than the memory.
    fn prune(&mut self, now: Instant) {
        self.outstanding
            .retain(|_, at| now.duration_since(*at) < OUTSTANDING_TIMEOUT);

        while let Some((id, at)) = self.consumed.front() {
            if self.consumed.len() <= CONSUMED_CAPACITY && now.duration_since(*at) < CONSUMED_MEMORY
            {
                break;
            }

            self.consumed_ids.remove(id);
            self.consumed.pop_front();
        }
    }
}

/// What one read of the mailbox produced.
enum Batch {
    /// Queued at least one command the previous read did not.
    Queued,
    /// Queued only commands the previous read had already handed out — a
    /// handler that keeps failing, coming straight back round. A
    /// [`Settlement::Retry`] makes a command eligible again on the very next
    /// read, so without parking on this a permanently failing command would
    /// spin the loop as fast as the db can answer, at a full `tb_peek` per
    /// turn.
    Repeated,
    /// Nothing to hand out — an empty mailbox, everything at the head is
    /// already outstanding, or the readable messages were all dead-lettered.
    Empty,
}

impl CommandStream {
    /// Open a command stream for `sri`. `poll_interval` is the backstop cadence
    /// for the subscription-driven wakeups; `batch_size` caps how many messages
    /// one read pulls in.
    pub async fn open(db: Client, sri: Sri, poll_interval: Duration, batch_size: usize) -> Self {
        let scope = scope_of_cell(sri);
        let polled =
            PolledTable::new(&db, models::Subject::Scope(scope.clone()), MESSAGES_TABLE).await;

        let mut interval = interval(poll_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let (settle, settled) = mpsc::unbounded_channel();

        Self {
            db,
            scope,
            polled,
            interval,
            batch_size: batch_size.max(1),
            queued: VecDeque::new(),
            dispatched: Dispatched::default(),
            settled,
            settle,
            reads_since_count: 0,
            // The first read is unprompted, like a backstop tick.
            woke: Woke::Backstop,
            prompted: false,
            notified_seen: 0,
            last_batch: HashSet::new(),
            metrics: Arc::new(CommandMetrics::new(sri)),
        }
    }

    /// Await the next command. Resolves to `None` only if the underlying db
    /// client is gone in a way that can't recover (currently never — errors
    /// back off and retry), so treat it as a stream that blocks until a command
    /// is available.
    pub async fn next(&mut self) -> Option<IncomingCommand> {
        loop {
            if let Some(next) = self.queued.pop_front() {
                return Some(next);
            }

            let settle = match self.poll_batch().await {
                Ok(Batch::Queued) => false,
                // Parked exactly like an empty read: the commands are queued
                // either way, so this only decides whether the loop pays for
                // the next read immediately or waits for a poke first.
                Ok(Batch::Repeated | Batch::Empty) => true,
                Err(err) => {
                    // Counted, not just logged: a read that fails looks exactly
                    // like an empty mailbox from here — the loop parks either
                    // way — so a run of failures is a silent stall. `tb_peek`
                    // retries a lost locate three times at up to
                    // `LOCATE_TIMEOUT` each before giving up, so one of these
                    // can cost more than a second on its own.
                    self.metrics.record_read_failed();
                    tracing::error!("unable to read the mailbox: {err}");
                    true
                }
            };

            if settle {
                let parked = Instant::now();
                self.woke = self.polled.wait(&mut self.interval).await;
                self.metrics.record_wait(parked.elapsed(), self.woke);
                self.prompted = true;
            }
        }
    }

    /// Reads up to `batch_size` messages from the head of the mailbox, queueing
    /// the ones that decode and dead-lettering the ones that don't.
    ///
    /// Walks on to the next window while everything read is suppressed and the
    /// window came back full — see [`SUPPRESSED_WINDOWS`].
    async fn poll_batch(&mut self) -> Result<Batch> {
        self.take_settlements();

        let now = Instant::now();
        let mut queued = HashSet::new();
        let mut cursor = None;

        for window in 0..=SUPPRESSED_WINDOWS {
            let (entities, mailbox_size) = self.read_head(cursor.take()).await?;
            if let Some(mailbox_size) = mailbox_size {
                self.metrics.record_depth(mailbox_size);
            }

            // The head is what a wakeup promised something in, so the poll
            // counters only ever describe the first window. Before any
            // filtering, too: whether the *read* found anything at all is a
            // different question from whether anything was dispatchable.
            if window == 0 {
                if std::mem::take(&mut self.prompted) {
                    self.metrics.record_poll(entities.is_empty(), self.woke);
                }

                let received = self.polled.received();
                self.metrics
                    .record_notifications(received.saturating_sub(self.notified_seen));
                self.notified_seen = received;
            }

            // Captured before the rows are consumed: a short window is the end
            // of the mailbox, and there is nothing behind it to walk on to.
            let full = entities.len() == self.batch_size;
            let last = entities.last().map(|(id, _)| id.clone());

            self.queue_window(entities, now, &mut queued).await;

            if !queued.is_empty() || !full {
                break;
            }

            let Some(last) = last else { break };
            cursor = Some(models::Cursor::After(last));
        }

        self.metrics.record_delivered(queued.len(), self.woke);

        let batch = if queued.is_empty() {
            Batch::Empty
        } else if queued.iter().any(|id| !self.last_batch.contains(id)) {
            Batch::Queued
        } else {
            Batch::Repeated
        };

        self.last_batch = queued;

        Ok(batch)
    }

    /// Queues the commands in one window of the head that aren't suppressed,
    /// dead-lettering the ones that don't decode. Records what it queued in
    /// `queued`.
    async fn queue_window(
        &mut self,
        entities: Vec<(models::Id, Vec<u8>)>,
        now: Instant,
        queued: &mut HashSet<models::Id>,
    ) {
        for (msg_id, value) in entities {
            if self.dispatched.suppresses(&msg_id, now) {
                continue;
            }

            match from_bytes::<MailboxCommand>(&value, "deserialise command") {
                Ok(command) => {
                    tracing::debug!(
                        "found command under {} : {}",
                        self.scope,
                        command.cmd.as_ref()
                    );
                    self.dispatched.dispatch(msg_id.clone(), now);
                    queued.insert(msg_id.clone());
                    self.queued.push_back(IncomingCommand {
                        receipt: CommandReceipt {
                            db: self.db.clone(),
                            scope: self.scope.clone(),
                            msg_id,
                            cmd: command.cmd.as_ref().to_owned(),
                            metrics: self.metrics.clone(),
                            settle: self.settle.clone(),
                        },
                        command,
                    });
                }
                Err(err) => {
                    tracing::error!(
                        "unable to deserialise command, moving to deadletter queue: {err}"
                    );
                    // Removing it is what stops it coming back; if that fails we
                    // simply see it again on the next read.
                    match self
                        .dead_letter(msg_id.clone(), value, err.to_string())
                        .await
                    {
                        Ok(()) => self.dispatched.settle(&msg_id, Settlement::Consumed, now),
                        Err(err) => {
                            tracing::error!("unable to add message to dead letter queue: {err}");
                        }
                    }
                }
            }
        }
    }

    /// Applies every outcome reported since the last read, and forgets the
    /// consumed ids old enough that no view can still be carrying them.
    ///
    /// This is the only thing that retires an id. Absence from a read is not
    /// evidence of anything: peeks are answered by whichever replica locate
    /// picks, so a row can be missing from one view and present in the next.
    fn take_settlements(&mut self) {
        let now = Instant::now();

        while let Ok((msg_id, settlement)) = self.settled.try_recv() {
            self.dispatched.settle(&msg_id, settlement, now);
        }

        self.dispatched.prune(now);
    }

    /// One routed round trip: `batch_size` rows from `cursor` — the head of the
    /// mailbox when it is `None` — plus, every [`COUNT_SAMPLE_EVERY`]th read,
    /// the table's total size for the queue-depth gauge (`None` on unsampled
    /// reads), read in a server-side snapshot.
    async fn read_head(
        &mut self,
        cursor: Option<models::Cursor>,
    ) -> Result<(Vec<(models::Id, Vec<u8>)>, Option<usize>)> {
        let sample_count = self.reads_since_count == 0;
        self.reads_since_count = (self.reads_since_count + 1) % COUNT_SAMPLE_EVERY;

        let started = Instant::now();
        let response = self
            .db
            .send(models::tb_peek::Request {
                scope: self.scope.clone(),
                table: String::from(MESSAGES_TABLE),
                cursor,
                limit: Some(self.batch_size),
                order: None,
                count: sample_count,
            })
            .await;
        self.metrics.record_peek(started.elapsed());

        let response = response
            .map_err(|err| Error::comm("mailbox read", err))?
            .map_err(|err| Error::db("mailbox read", err.message))?;

        Ok((response.entities, response.count))
    }

    /// Moves an undecodable message out of the mailbox and into the dead-letter
    /// table, atomically, in one routed round trip.
    async fn dead_letter(&self, msg_id: models::Id, value: Vec<u8>, reason: String) -> Result<()> {
        let letter = DeadLetter {
            reason,
            ty: DeadLetterType::Payload(value),
        };
        let encoded = to_bytes(&letter, "serialise dead letter")?;

        self.db
            .send(models::tx_apply::Request::commit_new(
                models::tx_begin::Constraint::Routed(self.scope.clone()),
                vec![
                    models::tb_append::Op {
                        scope: self.scope.clone(),
                        table: String::from(DEADLETTER_TABLE),
                        eid: Some(msg_id.clone()),
                        value: encoded,
                    }
                    .into(),
                    models::tb_delete::Op {
                        scope: self.scope.clone(),
                        table: String::from(MESSAGES_TABLE),
                        eid: msg_id,
                    }
                    .into(),
                ],
            ))
            .await
            .map_err(|err| Error::comm("dead-letter", err))?
            .map_err(|err| Error::db("dead-letter", err.message))?;

        Ok(())
    }
}

/// A command read from a cell's mailbox — still in it. See the module docs for
/// who removes it and when.
#[must_use = "a command stays in the mailbox until its receipt consumes it"]
pub struct IncomingCommand {
    command: MailboxCommand,
    receipt: CommandReceipt,
}

impl IncomingCommand {
    /// The command and its attachment metadata.
    pub fn command(&self) -> &MailboxCommand {
        &self.command
    }

    /// Removes the command in a transaction of its own. For readers that run
    /// none, and for which delivery is fire-and-forget either way.
    pub async fn consume(&self) -> Result<()> {
        self.receipt.consume().await
    }

    /// Split the command from the right to remove it, for a reader that hands
    /// the two to different places.
    pub fn into_parts(self) -> (MailboxCommand, CommandReceipt) {
        (self.command, self.receipt)
    }
}

/// The right to remove one command from a cell's mailbox, once whatever it
/// triggered is done.
pub struct CommandReceipt {
    db: Client,
    scope: models::Scope,
    msg_id: models::Id,
    cmd: String,
    metrics: Arc<CommandMetrics>,
    settle: mpsc::UnboundedSender<(models::Id, Settlement)>,
}

/// The right to report one command's outcome back to the stream that handed it
/// out. Split from the [`CommandReceipt`] because a cell folds the receipt into
/// the handler's transaction, but only learns whether that transaction
/// committed afterwards — the settlement has to outlive the receipt.
pub struct SettleHandle {
    msg_id: models::Id,
    cmd: String,
    metrics: Arc<CommandMetrics>,
    settle: mpsc::UnboundedSender<(models::Id, Settlement)>,
}

impl SettleHandle {
    /// Reports the outcome. Dropping the handle instead leaves the command
    /// outstanding until the stream's abandonment timeout.
    ///
    /// The only place the consumption counter is recorded, because this is the
    /// only place the delete is known to have committed.
    pub fn settle(self, settlement: Settlement) {
        if settlement == Settlement::Consumed {
            self.metrics.record_handled(&self.cmd);
        }

        let _ = self.settle.send((self.msg_id, settlement));
    }
}

impl CommandReceipt {
    /// A handle for reporting this command's outcome once it is known.
    #[must_use]
    pub fn settle_handle(&self) -> SettleHandle {
        SettleHandle {
            msg_id: self.msg_id.clone(),
            cmd: self.cmd.clone(),
            metrics: self.metrics.clone(),
            settle: self.settle.clone(),
        }
    }

    /// Buffers the removal of the command into `application` — the transaction
    /// of the work it triggered — so it is only consumed if that work commits,
    /// and costs no round trip of its own.
    ///
    /// Whether it did commit is only known later, so the consumption counter is
    /// not recorded here: it belongs to [`SettleHandle::settle`], which is told.
    pub fn consume_in_tx(&self, application: &mut Application) -> Result<()> {
        application
            .defer(models::tb_delete::Op {
                scope: self.scope.clone(),
                table: String::from(MESSAGES_TABLE),
                eid: self.msg_id.clone(),
            })
            .map_err(|err| Error::db("mailbox delete", err.message().to_string()))
    }

    /// Removes the command in a transaction of its own — one routed round trip
    /// instead of begin/delete/commit.
    pub async fn consume(&self) -> Result<()> {
        self.db
            .send(models::tx_apply::Request::commit_new(
                models::tx_begin::Constraint::Routed(self.scope.clone()),
                vec![
                    models::tb_delete::Op {
                        scope: self.scope.clone(),
                        table: String::from(MESSAGES_TABLE),
                        eid: self.msg_id.clone(),
                    }
                    .into(),
                ],
            ))
            .await
            .map_err(|err| Error::comm("mailbox delete", err))?
            .map_err(|err| Error::db("mailbox delete", err.message))?;

        self.settle_handle().settle(Settlement::Consumed);
        Ok(())
    }
}

/// Read and remove every command currently queued for `sri`, in a single
/// transaction. Undecodable entries are removed and skipped. Used for one-shot
/// clears (e.g. tearing down a gateway session's mailbox); the streaming
/// [`CommandStream`] is for ongoing consumption.
pub(crate) async fn drain_commands(db: &Client, sri: Sri) -> Result<Vec<MailboxCommand>> {
    let scope = scope_of_cell(sri);
    db.write_tx_in(scope.clone(), async move |client, tx_id| {
        Ok(drain_in_tx(client, tx_id, &scope).await)
    })
    .await
    .map_err(|err| Error::comm("drain commands", err))?
}

async fn drain_in_tx(
    client: &Client,
    tx_id: models::TxId,
    scope: &models::Scope,
) -> Result<Vec<MailboxCommand>> {
    let table = String::from(MESSAGES_TABLE);

    let listed = client
        .send(models::tb_list::Request {
            id: tx_id,
            op: models::tb_list::Op {
                scope: scope.clone(),
                table: table.clone(),
                cursor: None,
                limit: None,
                order: None,
            },
        })
        .await
        .map_err(|err| Error::comm("mailbox list", err))?
        .map_err(|err| Error::db("mailbox list", err.message))?;

    let mut commands = Vec::with_capacity(listed.entities.len());
    for (eid, value) in listed.entities {
        match from_bytes::<MailboxCommand>(&value, "decode mailbox command") {
            Ok(command) => commands.push(command),
            Err(err) => tracing::warn!("skipping undecodable mailbox entry: {err}"),
        }
        client
            .send(models::tb_delete::Request {
                id: tx_id,
                op: models::tb_delete::Op {
                    scope: scope.clone(),
                    table: table.clone(),
                    eid,
                },
            })
            .await
            .map_err(|err| Error::comm("mailbox delete", err))?
            .map_err(|err| Error::db("mailbox delete", err.message))?;
    }

    Ok(commands)
}

struct CommandMetrics {
    mailbox_size: Gauge<u64>,
    /// The same sampled depth as counters (total and sample count), because
    /// the file exporter's snapshot only carries a gauge's *last* value — by
    /// collection time the drained mailbox reads ~0 and the pass's depths are
    /// gone. A mean over the sampled reads survives: it is what the node
    /// serving `tb_peek` said its whole table held, so a large mean with
    /// small dispatched batches means the head of the window is jammed with
    /// rows this reader already dispatched, while a small mean means the rows
    /// simply are not where the peek resolved.
    depth_sum: Counter<u64>,
    depth_samples: Counter<u64>,
    queued: Counter<u64>,
    polls: Counter<u64>,
    empty_polls: Counter<u64>,
    backstop_polls: Counter<u64>,
    backstop_empty_polls: Counter<u64>,
    notifications: Counter<u64>,
    delivered_poke: Counter<u64>,
    delivered_backstop: Counter<u64>,
    peek_nanos: Counter<u64>,
    peeks: Counter<u64>,
    wait_nanos: Counter<u64>,
    waits: Counter<u64>,
    backstop_wait_nanos: Counter<u64>,
    backstop_waits: Counter<u64>,
    read_failures: Counter<u64>,
    sri: KeyValue,
    kind: KeyValue,
    pid: KeyValue,
}

impl CommandMetrics {
    fn new(sri: Sri) -> Self {
        let meter = opentelemetry::global::meter("cell_interaction");
        Self {
            mailbox_size: meter.u64_gauge("cell_mailbox_size").build(),
            depth_sum: meter.u64_counter("cell_mailbox_depth_sum").build(),
            depth_samples: meter.u64_counter("cell_mailbox_depth_samples").build(),
            // Not `cell_commands_processed`: that name is `sorg_execution::wasm::cell::cell_task`'s
            // own counter for a message having run all the way through the cell's handler loop
            // (see that module's doc comment, which already names this one — as `queued` — as its
            // distinct companion). Using the same name here as well double-counted every command
            // under `cell_commands_processed`, since both counters fire once per command; a reader
            // aggregating by that name (e.g. `CellInteractionMetricsSnapshot`) saw double the real
            // total.
            queued: meter.u64_counter("cell_commands_queued").build(),
            // A hop between cells costs a flat ~18ms on the rack, which the
            // gigabit link, the tmpfs store and 14us handlers cannot account
            // for. The suspicion these two test: the sender's `tb_append` runs
            // on the *sender's* node (the handler's application is routed by
            // the sender's own scope), so a poke can arrive before the row is
            // visible to a locate that resolves to the recipient's replica. If
            // that is what happens, most wakes poll an empty mailbox and the
            // row only lands on a later wake — `empty_polls / polls` says so
            // directly, and near-zero exonerates the theory.
            polls: meter.u64_counter("cell_mailbox_polls").build(),
            empty_polls: meter.u64_counter("cell_mailbox_empty_polls").build(),
            // The same pair for backstop-driven reads. Not interesting on
            // their own — an idle cell polls an empty mailbox every 5s — but
            // needed as the denominator for the two below.
            backstop_polls: meter.u64_counter("cell_mailbox_backstop_polls").build(),
            backstop_empty_polls: meter
                .u64_counter("cell_mailbox_backstop_empty_polls")
                .build(),
            // Notifications as delivered, before `Notify` collapses them into
            // wakeups. Against `cell_commands_received` this is the poke loss
            // rate: one row appended into a mailbox publishes exactly one event
            // on that mailbox's table, and deletes publish none, so the two
            // should match. Table events are zenoh pushes, whose default
            // congestion control is `Drop` at the default `Data` priority —
            // the same class the blocking queries occupy — so a congested link
            // discards them silently.
            notifications: meter.u64_counter("cell_mailbox_notifications").build(),
            // Which signal actually delivered each command. A row whose poke
            // was dropped waits for the 5s backstop, and one whose replication
            // announce was dropped waits 2.1-8.0s for the next periodic
            // announce; load 1000's median of 1671ms with a 4017ms p95 fits a
            // mixture of prompt delivery and one of those, so this splits it.
            delivered_poke: meter.u64_counter("cell_commands_delivered_poke").build(),
            delivered_backstop: meter
                .u64_counter("cell_commands_delivered_backstop")
                .build(),
            // Wall time inside the `tb_peek` round trip, as a sum and a count
            // rather than a histogram: the harness reads counters, and the
            // question here is what a round trip *costs*, which a mean answers.
            //
            // The zone tier's ceiling works out at one blocking round trip per
            // command (13us of handler against the rest), so inverting its
            // measured 130-150 cmd/s/cell implies 6.7-7.7ms per trip. That is
            // derived, not observed, and it is enormous for gigabit ethernet
            // against a tmpfs store — an earlier campaign measured ~1.2ms per
            // query. These two pairs replace the inference with the number.
            peek_nanos: meter.u64_counter("cell_peek_nanos").build(),
            peeks: meter.u64_counter("cell_peeks").build(),
            // How long the loop sat parked, split by what woke it. Rows now
            // measurably arrive at the recipient's holder within 24ms of being
            // written (run 33444574293) while a hop takes 1600ms, so the wait is
            // entirely on this side. Either the cell is not being woken — a long
            // notified wait, meaning pokes are not arriving even though the data
            // is here — or it is woken promptly and the delay is in what it does
            // next.
            wait_nanos: meter.u64_counter("cell_wait_nanos").build(),
            waits: meter.u64_counter("cell_waits").build(),
            backstop_wait_nanos: meter.u64_counter("cell_backstop_wait_nanos").build(),
            backstop_waits: meter.u64_counter("cell_backstop_waits").build(),
            // A failed read parks the loop exactly like an empty one, so this is
            // the difference between an idle cell and a stalled one.
            read_failures: meter.u64_counter("cell_read_failures").build(),
            sri: KeyValue::new("sri", sri.to_string()),
            kind: KeyValue::new("kind", "command"),
            pid: KeyValue::new("pid", std::process::id().to_string()),
        }
    }

    /// The queue depth as of one read of the mailbox — recorded once per read,
    /// not once per command in it.
    fn record_depth(&self, mailbox_count: usize) {
        let attrs = [self.sri.clone(), self.kind.clone(), self.pid.clone()];
        self.mailbox_size.record(mailbox_count as u64, &attrs);
        self.depth_sum.add(mailbox_count as u64, &attrs);
        self.depth_samples.add(1, &attrs);
    }

    /// One read of the mailbox, counted against why the reader woke: a read
    /// that finds nothing *after being told a row landed* means the write is
    /// not yet visible where the read resolved, while the same read after a
    /// backstop tick is an ordinary idle poll.
    fn record_poll(&self, empty: bool, woke: Woke) {
        let attrs = [self.sri.clone(), self.kind.clone(), self.pid.clone()];

        let (polls, empty_polls) = match woke {
            Woke::Notified => (&self.polls, &self.empty_polls),
            Woke::Backstop => (&self.backstop_polls, &self.backstop_empty_polls),
        };

        polls.add(1, &attrs);
        if empty {
            empty_polls.add(1, &attrs);
        }
    }

    /// Commands one read queued, attributed to the signal that woke the reader.
    /// A burst drained by repeated reads is all attributed to the wakeup that
    /// started it, which is the intent: the question is which signal got the
    /// reader moving, not how many reads it then took.
    fn record_delivered(&self, count: usize, woke: Woke) {
        if count == 0 {
            return;
        }

        let counter = match woke {
            Woke::Notified => &self.delivered_poke,
            Woke::Backstop => &self.delivered_backstop,
        };

        counter.add(
            count as u64,
            &[self.sri.clone(), self.kind.clone(), self.pid.clone()],
        );
    }

    /// One `tb_peek` round trip, however it turned out — a failed trip cost the
    /// same wall time as a successful one.
    fn record_peek(&self, elapsed: Duration) {
        let attrs = [self.sri.clone(), self.kind.clone(), self.pid.clone()];
        self.peek_nanos.add(
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
            &attrs,
        );
        self.peeks.add(1, &attrs);
    }

    /// One park of the loop, and how it ended.
    fn record_wait(&self, waited: Duration, woke: Woke) {
        let attrs = [self.sri.clone(), self.kind.clone(), self.pid.clone()];
        let nanos = u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX);

        let (total, count) = match woke {
            Woke::Notified => (&self.wait_nanos, &self.waits),
            Woke::Backstop => (&self.backstop_wait_nanos, &self.backstop_waits),
        };

        total.add(nanos, &attrs);
        count.add(1, &attrs);
    }

    /// One mailbox read that errored out rather than coming back empty.
    fn record_read_failed(&self) {
        self.read_failures
            .add(1, &[self.sri.clone(), self.kind.clone(), self.pid.clone()]);
    }

    /// Notifications delivered since the last read, taken from the watcher's
    /// own pre-coalescing count.
    fn record_notifications(&self, delta: u64) {
        if delta == 0 {
            return;
        }

        self.notifications.add(
            delta,
            &[self.sri.clone(), self.kind.clone(), self.pid.clone()],
        );
    }

    /// One command consumed. Recorded on consumption rather than on read, so a
    /// command redelivered after a failed handler is not counted twice.
    fn record_handled(&self, cmd_name: &str) {
        self.queued.add(
            1,
            &[
                self.sri.clone(),
                self.kind.clone(),
                KeyValue::new("cmd", cmd_name.to_owned()),
                self.pid.clone(),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> models::Id {
        vec![n]
    }

    #[test]
    fn a_dispatched_command_is_not_handed_out_again() {
        let now = Instant::now();
        let mut dispatched = Dispatched::default();

        dispatched.dispatch(id(1), now);

        assert!(dispatched.suppresses(&id(1), now));
        assert!(!dispatched.suppresses(&id(2), now));
    }

    #[test]
    fn a_consumed_command_stays_suppressed() {
        // The regression this whole mechanism exists for: the delete committed,
        // but a peek answered from a replica that has not applied it yet still
        // carries the row. Handing it out again is a duplicate handler run.
        let now = Instant::now();
        let mut dispatched = Dispatched::default();

        dispatched.dispatch(id(1), now);
        dispatched.settle(&id(1), Settlement::Consumed, now);

        let stale_view = now + Duration::from_secs(2);
        dispatched.prune(stale_view);
        assert!(dispatched.suppresses(&id(1), stale_view));
    }

    #[test]
    fn a_retried_command_is_eligible_at_once() {
        let now = Instant::now();
        let mut dispatched = Dispatched::default();

        dispatched.dispatch(id(1), now);
        dispatched.settle(&id(1), Settlement::Retry, now);

        assert!(!dispatched.suppresses(&id(1), now));
    }

    #[test]
    fn a_consumed_command_is_eligible_again_once_no_view_can_carry_it() {
        // At-least-once has to survive a delete that never actually landed.
        let now = Instant::now();
        let mut dispatched = Dispatched::default();

        dispatched.dispatch(id(1), now);
        dispatched.settle(&id(1), Settlement::Consumed, now);

        let expired = now + CONSUMED_MEMORY + Duration::from_secs(1);
        dispatched.prune(expired);
        assert!(!dispatched.suppresses(&id(1), expired));
    }

    #[test]
    fn an_unsettled_command_is_redelivered_after_the_abandonment_timeout() {
        let now = Instant::now();
        let mut dispatched = Dispatched::default();

        dispatched.dispatch(id(1), now);

        let abandoned = now + OUTSTANDING_TIMEOUT + Duration::from_secs(1);
        assert!(!dispatched.suppresses(&id(1), abandoned));
    }

    #[test]
    fn consumed_ids_are_capped() {
        let now = Instant::now();
        let mut dispatched = Dispatched::default();

        for n in 0..=u16::try_from(CONSUMED_CAPACITY).expect("capacity fits") {
            let id = n.to_le_bytes().to_vec();
            dispatched.dispatch(id.clone(), now);
            dispatched.settle(&id, Settlement::Consumed, now);
        }
        dispatched.prune(now);

        assert_eq!(dispatched.consumed.len(), CONSUMED_CAPACITY);
        assert_eq!(dispatched.consumed_ids.len(), CONSUMED_CAPACITY);
        // The oldest is the one that went.
        assert!(!dispatched.suppresses(&0u16.to_le_bytes().to_vec(), now));
    }

    #[test]
    fn an_abandoned_command_stops_costing_a_map_entry() {
        // Only a settlement removes an outstanding entry, so a receipt that is
        // never settled — a cell task killed mid-turn — used to leak one for
        // the life of the cell. It stops suppressing at the timeout either way.
        let now = Instant::now();
        let mut dispatched = Dispatched::default();

        dispatched.dispatch(id(1), now);
        dispatched.dispatch(id(2), now);

        dispatched.prune(now);
        assert_eq!(dispatched.outstanding.len(), 2, "still within the window");

        let abandoned = now + OUTSTANDING_TIMEOUT + Duration::from_secs(1);
        dispatched.prune(abandoned);

        assert!(dispatched.outstanding.is_empty());
        assert!(!dispatched.suppresses(&id(1), abandoned));
    }

    #[test]
    fn settling_the_same_id_twice_keeps_one_tombstone() {
        let now = Instant::now();
        let mut dispatched = Dispatched::default();

        dispatched.settle(&id(1), Settlement::Consumed, now);
        dispatched.settle(&id(1), Settlement::Consumed, now);

        assert_eq!(dispatched.consumed.len(), 1);
    }
}
