//! Live view of the replication channels: subscribes to the replica topic and
//! prints every message moving between nodes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use cell_protocol::replication::ReplicaSelector;
use cell_protocol::{PLACEMENT_TABLE, PlacementEntry, Sri, placement_scope};
use db_client::v1::models;
use db_commons::models::replication::{Announce, ChangeSet, ChangeSetReq, Probe, SyncMarker};
use db_commons::models::{ReplicaMessage, Subject};
use db_commons::topics::replica;
use human_bytes::human_bytes;

use crate::args::Ctx;
use crate::render::RESET;
use crate::{info, warn};

/// Foreground colour for a head count.
const HEADS: &str = "\x1b[1;32m";

/// Foreground colour for a byte size.
const SIZE: &str = "\x1b[33m";

/// A spread of readable 256-colour codes; a scope hashes into one of these so
/// the same scope keeps its hue through the stream and different scopes stay
/// easy to tell apart.
const SCOPE_PALETTE: [&str; 16] = [
    "\x1b[38;5;39m",
    "\x1b[38;5;208m",
    "\x1b[38;5;77m",
    "\x1b[38;5;170m",
    "\x1b[38;5;214m",
    "\x1b[38;5;45m",
    "\x1b[38;5;141m",
    "\x1b[38;5;178m",
    "\x1b[38;5;111m",
    "\x1b[38;5;205m",
    "\x1b[38;5;149m",
    "\x1b[38;5;117m",
    "\x1b[38;5;216m",
    "\x1b[38;5;84m",
    "\x1b[38;5;173m",
    "\x1b[38;5;219m",
];

/// Watch the replication channels and log every message as it moves.
///
/// Prints each probe, announce, changeset request, and changeset seen on the
/// network: who sent it, what scope it concerns, and how much data moved.
/// Runs until interrupted. `-v` adds per-head, per-floor, and per-entry
/// detail; `-e` hides channels from the output.
#[derive(clap::Parser)]
pub struct Monitor {
    /// What to watch: `app:<name>` for a whole application, a cell's SRI (a
    /// UUID) or SRN (`chatty`, `chatty/server`) for one cell, or
    /// `scope:ns[/db[/schema]]` for a slice of the scope hierarchy.
    /// With no identifiers, every replication channel is monitored.
    #[clap(value_name = "IDENTIFIER")]
    targets: Vec<String>,

    /// Hide what this identifier names; same forms as the positional
    /// identifiers, repeatable.
    #[clap(short, long, value_name = "IDENTIFIER")]
    exclude: Vec<String>,

    /// Aggregate message and byte rates instead of printing each message,
    /// summarised every `--interval` seconds.
    #[clap(long)]
    stats: bool,

    /// Seconds between rate summaries.
    #[clap(long, default_value_t = 5, requires = "stats")]
    interval: u64,
}

pub async fn handle(ctx: Ctx, cmd: Monitor) -> Result<()> {
    let selectors = parse(&cmd.targets)?;
    let excludes = parse(&cmd.exclude)?;

    let session = ctx.session().await?;
    let filter = resolve(ctx, &session, &selectors, &excludes).await?;

    if !selectors.is_empty() && filter.include.is_empty() {
        anyhow::bail!("nothing to monitor: the given identifiers name no cells");
    }
    if !excludes.is_empty() && filter.exclude.is_empty() {
        warn!(
            ctx,
            "the excluded identifiers name no cells; nothing is hidden"
        );
    }

    let (_subscribers, mut receiver) = subscribe_replica_topics(&session, &filter).await?;

    let excluding = if filter.exclude.is_empty() {
        String::new()
    } else {
        format!(", excluding {}", list(&filter.exclude))
    };
    if filter.include.is_empty() {
        info!(
            ctx,
            "monitoring all replication channels{excluding} (ctrl-c to stop)"
        );
    } else {
        info!(
            ctx,
            "monitoring replication of {}{excluding} (ctrl-c to stop)",
            list(&filter.include)
        );
    }

    if cmd.stats {
        return run_stats(ctx, &mut receiver, &filter, cmd.interval).await;
    }

    let styled = std::io::stdout().is_terminal();

    while let Some((received, keyexpr, payload)) = receiver.recv().await {
        let sender = match replica::parse_sender(&keyexpr) {
            Ok(id) => id.to_string(),
            Err(err) => {
                warn!(ctx, "cannot parse sender from {keyexpr}: {err}");
                continue;
            }
        };

        match postcard::from_bytes::<ReplicaMessage>(&payload) {
            Ok(msg) => {
                let detail = ctx.verbose >= 1;
                if let Some(text) = render(
                    received,
                    &sender,
                    payload.len(),
                    &msg,
                    &filter,
                    detail,
                    styled,
                ) {
                    print!("{text}");
                }
            }
            Err(err) => warn!(
                ctx,
                "undecodable replica message from {sender} on {keyexpr}: {err}"
            ),
        }
    }

    Ok(())
}

/// One subscriber per included subject (or a catch-all), all feeding a single
/// channel of `(received, keyexpr, payload)`.
///
/// Publishers format unspecified subject levels as `*`, so a concrete level
/// here still intersects their keyexprs; the leftover breadth is trimmed by
/// the filter when rendering. Exclusions only hide output: subscribing
/// more narrowly than the includes isn't possible with keyexprs.
pub(super) async fn subscribe_replica_topics(
    session: &zenoh::Session,
    filter: &Filter,
) -> Result<(
    Vec<zenoh::pubsub::Subscriber<()>>,
    tokio::sync::mpsc::UnboundedReceiver<(SystemTime, String, Vec<u8>)>,
)> {
    let keyexprs: BTreeSet<String> = if filter.include.is_empty() {
        BTreeSet::from([replica::format_replica("*", "*", "*", "*", "*")])
    } else {
        filter
            .include
            .iter()
            .map(|subject| {
                let (namespace, database, schema) = subject.as_keyexprs();
                replica::format_replica(namespace, database, schema, "*", "*")
            })
            .collect()
    };

    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

    let mut subscribers = Vec::with_capacity(keyexprs.len());
    for keyexpr in keyexprs {
        let sender = sender.clone();
        let subscriber = session
            .declare_subscriber(keyexpr.as_str())
            .callback(move |sample| {
                let received = SystemTime::now();
                let keyexpr = sample.key_expr().to_string();
                let payload = sample.payload().to_bytes().into_owned();
                let _ = sender.send((received, keyexpr, payload));
            })
            .await
            .map_err(|err| anyhow::anyhow!("unable to subscribe to {keyexpr}: {err}"))?;
        subscribers.push(subscriber);
    }

    Ok((subscribers, receiver))
}

/// Counts traffic into windows and prints a rate table per window.
async fn run_stats(
    ctx: Ctx,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<(SystemTime, String, Vec<u8>)>,
    filter: &Filter,
    interval: u64,
) -> Result<()> {
    let mut window = StatsWindow::new();

    let mut ticker = tokio::time::interval(Duration::from_secs(interval.max(1)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // the interval's immediate first tick

    loop {
        tokio::select! {
            received = receiver.recv() => {
                let Some((_, keyexpr, payload)) = received else { break };

                let sender = match replica::parse_sender(&keyexpr) {
                    Ok(id) => id.to_string(),
                    Err(err) => {
                        warn!(ctx, "cannot parse sender from {keyexpr}: {err}");
                        continue;
                    }
                };

                match postcard::from_bytes::<ReplicaMessage>(&payload) {
                    Ok(msg) => window.record(&sender, payload.len(), &msg, filter),
                    Err(err) => warn!(
                        ctx,
                        "undecodable replica message from {sender} on {keyexpr}: {err}"
                    ),
                }
            }
            _ = ticker.tick() => {
                print!("{}", window.render());
                window.reset();
            }
        }
    }

    Ok(())
}

/// One reporting window of replication traffic, counted per
/// (message type, scope, sender).
struct StatsWindow {
    started: Instant,
    rows: BTreeMap<(String, String, String), Counter>,
}

#[derive(Default)]
struct Counter {
    msgs: u64,
    bytes: u64,
}

impl StatsWindow {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            rows: BTreeMap::new(),
        }
    }

    fn reset(&mut self) {
        self.started = Instant::now();
        self.rows.clear();
    }

    /// Counts `msg` against its row; messages the filter rules out entirely
    /// are ignored. Announces and probes span scopes, so they count under
    /// `-` rather than one scope.
    fn record(&mut self, sender: &str, size: usize, msg: &ReplicaMessage, filter: &Filter) {
        let scope = match msg {
            ReplicaMessage::Probe(probe) => {
                let concerns_us =
                    probe.filter.is_empty() || probe.filter.iter().any(|s| filter.admits(s));
                if !concerns_us {
                    return;
                }
                String::from("-")
            }
            ReplicaMessage::Announce(announce) => {
                let concerns_us = filter.is_empty()
                    || announce.known.iter().any(|(scope, _)| filter.admits(scope));
                if !concerns_us {
                    return;
                }
                String::from("-")
            }
            ReplicaMessage::ChangeSetReq(req) => {
                if !filter.admits(&req.scope) {
                    return;
                }
                req.scope.to_string()
            }
            ReplicaMessage::ChangeSet(cs) => {
                if !filter.admits(&cs.scope) {
                    return;
                }
                cs.scope.to_string()
            }
        };

        let key = (
            String::from(msg.name()),
            scope,
            String::from(prefix(sender)),
        );
        let row = self.rows.entry(key).or_default();
        row.msgs += 1;
        row.bytes += size as u64;
    }

    fn render(&self) -> String {
        self.render_for(SystemTime::now(), self.started.elapsed())
    }

    fn render_for(&self, now: SystemTime, elapsed: Duration) -> String {
        let now = humantime::format_rfc3339_seconds(now);
        let secs = elapsed.as_secs_f64().max(0.001);

        if self.rows.is_empty() {
            return format!("{now}  no replication traffic in the last {secs:.1}s\n");
        }

        let mut rows: Vec<(&(String, String, String), &Counter)> = self.rows.iter().collect();
        rows.sort_by(|(a_key, a), (b_key, b)| b.bytes.cmp(&a.bytes).then_with(|| a_key.cmp(b_key)));

        let total = rows
            .iter()
            .fold(Counter::default(), |acc, (_, row)| Counter {
                msgs: acc.msgs + row.msgs,
                bytes: acc.bytes + row.bytes,
            });

        let mut table = vec![[
            String::from("MESSAGE"),
            String::from("SCOPE"),
            String::from("SENDER"),
            String::from("MSGS"),
            String::from("MSG/S"),
            String::from("BYTES"),
            String::from("BYTES/S"),
        ]];
        for ((name, scope, sender), row) in rows {
            table.push(cells(name, scope, sender, row, secs));
        }
        table.push(cells("TOTAL", "", "", &total, secs));

        let widths: Vec<usize> = (0..7)
            .map(|col| crate::render::width(table.iter().map(|row| row[col].as_str())))
            .collect();

        let mut out = format!(
            "{now}  {} msg(s), {} in {secs:.1}s\n",
            total.msgs,
            size(total.bytes),
        );
        for row in &table {
            let [name, scope, sender, msgs, msg_rate, bytes, byte_rate] = row;
            let _ = writeln!(
                out,
                "{name:<w0$}  {scope:<w1$}  {sender:<w2$}  {msgs:>w3$}  {msg_rate:>w4$}  {bytes:>w5$}  {byte_rate:>w6$}",
                w0 = widths[0],
                w1 = widths[1],
                w2 = widths[2],
                w3 = widths[3],
                w4 = widths[4],
                w5 = widths[5],
                w6 = widths[6],
            );
        }
        out.push('\n');
        out
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn cells(name: &str, scope: &str, sender: &str, row: &Counter, secs: f64) -> [String; 7] {
    [
        String::from(name),
        String::from(scope),
        String::from(sender),
        row.msgs.to_string(),
        format!("{:.1}", row.msgs as f64 / secs),
        size(row.bytes),
        format!("{}/s", size((row.bytes as f64 / secs) as u64)),
    ]
}

#[allow(clippy::cast_precision_loss)]
fn size(bytes: u64) -> String {
    human_bytes(bytes as f64)
}

/// What the identifiers admit: a message must touch an included subject (when
/// any are given) and no excluded one.
pub(super) struct Filter {
    pub(super) include: Vec<Subject>,
    pub(super) exclude: Vec<Subject>,
}

impl Filter {
    pub(super) fn admits(&self, scope: &models::Scope) -> bool {
        (self.include.is_empty() || self.include.iter().any(|subject| subject.contains(scope)))
            && !self.exclude.iter().any(|subject| subject.contains(scope))
    }

    fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }
}

pub(super) fn parse(identifiers: &[String]) -> Result<Vec<ReplicaSelector>> {
    identifiers
        .iter()
        .map(|identifier| {
            identifier
                .parse()
                .map_err(|err| anyhow::anyhow!("invalid identifier '{identifier}': {err}"))
        })
        .collect()
}

/// Expands the identifiers into subjects, reading the placements only when an
/// `app:` identifier makes them necessary.
pub(super) async fn resolve(
    ctx: Ctx,
    session: &zenoh::Session,
    selectors: &[ReplicaSelector],
    excludes: &[ReplicaSelector],
) -> Result<Filter> {
    let placements = if selectors
        .iter()
        .chain(excludes)
        .any(|selector| matches!(selector, ReplicaSelector::App(_)))
    {
        read_placements(ctx, session).await?
    } else {
        Vec::new()
    };

    let cells: Vec<(&Sri, Option<&str>)> = placements
        .iter()
        .map(|cell| (&cell.sri, cell.app.as_deref()))
        .collect();

    Ok(Filter {
        include: expand(selectors, &cells),
        exclude: expand(excludes, &cells),
    })
}

fn expand(selectors: &[ReplicaSelector], cells: &[(&Sri, Option<&str>)]) -> Vec<Subject> {
    let mut subjects = Vec::new();
    for selector in selectors {
        for subject in selector.subjects(cells.iter().copied()) {
            if !subjects.contains(&subject) {
                subjects.push(subject);
            }
        }
    }
    subjects
}

fn list(subjects: &[Subject]) -> String {
    subjects
        .iter()
        .map(|subject| {
            let (namespace, database, schema) = subject.as_keyexprs();
            format!("{namespace}/{database}/{schema}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Only the placements can say which cells an application owns.
async fn read_placements(ctx: Ctx, session: &zenoh::Session) -> Result<Vec<PlacementEntry>> {
    let db = db_client::v1::Client::new(session);

    let response = db
        .read_tx_in(placement_scope(), async move |client, tx_id| {
            client
                .send(models::tb_list::Request {
                    id: tx_id,
                    op: models::tb_list::Op {
                        scope: placement_scope(),
                        table: String::from(PLACEMENT_TABLE),
                        cursor: None,
                        limit: None,
                        order: None,
                    },
                })
                .await
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to communicate with db: {err}"))?
        .map_err(|err| anyhow::anyhow!("unable to list the placements: {}", err.message))?;

    Ok(response
        .entities
        .iter()
        .filter_map(|(id, value)| match postcard::from_bytes(value) {
            Ok(row) => Some(row),
            Err(err) => {
                warn!(
                    ctx,
                    "skipping undecodable placement row [{}]: {err}",
                    String::from_utf8_lossy(id)
                );
                None
            }
        })
        .collect())
}

/// Renders one message, or `None` when the filter rules it out.
fn render(
    received: SystemTime,
    sender: &str,
    size: usize,
    msg: &ReplicaMessage,
    filter: &Filter,
    detail: bool,
    styled: bool,
) -> Option<String> {
    let header = format!(
        "{} {:<8} {:<13}",
        humantime::format_rfc3339_millis(received),
        prefix(sender),
        msg.name(),
    );

    match msg {
        ReplicaMessage::Probe(probe) => render_probe(&header, probe, filter, styled),
        ReplicaMessage::Announce(announce) => {
            render_announce(&header, announce, size, filter, detail, styled)
        }
        ReplicaMessage::ChangeSetReq(req) => render_req(&header, req, filter, detail, styled),
        ReplicaMessage::ChangeSet(cs) => {
            render_changeset(&header, cs, size, filter, detail, styled)
        }
    }
}

fn render_probe(header: &str, probe: &Probe, filter: &Filter, styled: bool) -> Option<String> {
    let probed = &probe.filter;
    // An empty probe filter asks about everything, so it always concerns us.
    if !probed.is_empty() && !probed.iter().any(|scope| filter.admits(scope)) {
        return None;
    }

    let mut out = String::new();
    if probed.is_empty() {
        let _ = writeln!(out, "{header} all scopes");
    } else {
        let scopes = probed
            .iter()
            .map(|scope| scope_label(scope, styled))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "{header} {scopes}");
    }
    Some(out)
}

fn render_announce(
    header: &str,
    announce: &Announce,
    size: usize,
    filter: &Filter,
    detail: bool,
    styled: bool,
) -> Option<String> {
    let known = &announce.known;
    let shown: Vec<_> = known
        .iter()
        .filter(|(scope, _)| filter.admits(scope))
        .collect();
    if shown.is_empty() && !filter.is_empty() {
        return None;
    }

    let total = known.len();
    let count = if shown.len() == total {
        format!("{total} scope(s)")
    } else {
        format!("{} of {total} scope(s)", shown.len())
    };

    let role = if announce.full_replica {
        ""
    } else {
        " (offloader)"
    };

    let mut out = String::new();
    let _ = writeln!(out, "{header} {count}{role}, {}", bytes(size, styled));

    for (scope, sa) in shown {
        let scope = scope_label(scope, styled);
        if detail {
            if let Some(baseline) = sa.baseline {
                let _ = writeln!(
                    out,
                    "  {scope}  baseline {} (fingerprint {:016x})",
                    version(baseline),
                    sa.fingerprint,
                );
            }
            for (ts, (epoch, node)) in &sa.heads {
                let _ = writeln!(
                    out,
                    "  {scope}  {} (epoch {epoch}, node {})",
                    version(*ts),
                    node_id(node),
                );
            }
        }

        if !detail || sa.heads.is_empty() {
            let baseline = match sa.baseline {
                Some(baseline) => format!(", baseline {}", version(baseline)),
                None => String::new(),
            };
            match sa.heads.last_key_value() {
                Some((ts, (epoch, node))) => {
                    let _ = writeln!(
                        out,
                        "  {scope}  {} head(s), latest {} (epoch {epoch}, node {}){baseline}",
                        heads(sa.heads.len(), styled),
                        version(*ts),
                        node_id(node),
                    );
                }
                None => {
                    let _ = writeln!(out, "  {scope}  no explicit heads{baseline}");
                }
            }
        }
    }
    Some(out)
}

fn render_req(
    header: &str,
    req: &ChangeSetReq,
    filter: &Filter,
    detail: bool,
    styled: bool,
) -> Option<String> {
    if !filter.admits(&req.scope) {
        return None;
    }

    let since = match req.since_ts {
        Some(ts) => version(ts).to_string(),
        None => String::from("the beginning"),
    };

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{header} {}  since {since}, {} epoch floor(s)",
        scope_label(&req.scope, styled),
        req.epoch_floors.len(),
    );

    if detail {
        for (ts, epoch) in &req.epoch_floors {
            let _ = writeln!(out, "  {} above epoch {epoch}", version(*ts));
        }
    }
    Some(out)
}

fn render_changeset(
    header: &str,
    cs: &ChangeSet,
    size: usize,
    filter: &Filter,
    detail: bool,
    styled: bool,
) -> Option<String> {
    if !filter.admits(&cs.scope) {
        return None;
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{header} {}  {} chunk(s), {}",
        scope_label(&cs.scope, styled),
        cs.chunks.len(),
        bytes(size, styled),
    );

    for chunk in &cs.chunks {
        let (epoch, ts, node) = chunk.id;
        let marker = match chunk.meta.marker {
            SyncMarker::Mutation => "mutation",
            SyncMarker::Deletion => "deletion",
        };
        let _ = writeln!(
            out,
            "  {marker} @ {} (epoch {epoch}, node {})  {} entries",
            version(ts),
            node_id(&node),
            chunk.entries.len(),
        );

        if detail {
            for (key, value) in &chunk.entries {
                match value {
                    Some(value) => {
                        let _ = writeln!(
                            out,
                            "    put {} ({})",
                            key.escape_ascii(),
                            bytes(value.len(), styled)
                        );
                    }
                    None => {
                        let _ = writeln!(out, "    del {}", key.escape_ascii());
                    }
                }
            }
        }
    }
    Some(out)
}

/// Enough of a hex id to tell nodes apart in a stream.
pub(super) fn prefix(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn node_id(id: &models::NodeId) -> String {
    let mut full = uhlc::ID::try_from(id).map_or_else(|_| String::from("0"), |id| id.to_string());
    full.truncate(8);
    full
}

/// An HLC version as wall-clock time; nanoseconds keep distinct versions
/// visibly distinct.
pub(super) fn version(ts: models::Version) -> impl std::fmt::Display {
    humantime::format_rfc3339_nanos(SystemTime::UNIX_EPOCH + uhlc::NTP64(ts).to_duration())
}

fn bytes(size: usize, styled: bool) -> String {
    #[allow(clippy::cast_precision_loss)]
    let rendered = human_bytes(size as f64);
    if styled {
        format!("{SIZE}{rendered}{RESET}")
    } else {
        rendered
    }
}

/// A head count, coloured so it stands out from the surrounding text.
fn heads(count: usize, styled: bool) -> String {
    if styled {
        format!("{HEADS}{count}{RESET}")
    } else {
        count.to_string()
    }
}

/// A scope in its stable colour; plain text when unstyled.
pub(super) fn scope_label(scope: &models::Scope, styled: bool) -> String {
    let text = scope.to_string();
    if styled {
        format!("{}{text}{RESET}", scope_palette(&text))
    } else {
        text
    }
}

/// Hashes a scope into [`SCOPE_PALETTE`] (FNV-1a) so it always paints the same.
fn scope_palette(scope: &str) -> &'static str {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in scope.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    SCOPE_PALETTE[hash as usize % SCOPE_PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use db_commons::models::replication::{Chunk, ScopeAnnounce, SyncMeta, VecMap};

    use super::*;

    const NODE: models::NodeId = [7; 16];

    fn scope(namespace: &str) -> models::Scope {
        models::Scope::new(namespace, "db", "p")
    }

    fn changeset(namespace: &str, entries: Vec<(Vec<u8>, Option<Vec<u8>>)>) -> ReplicaMessage {
        ReplicaMessage::ChangeSet(ChangeSet {
            tx_id: None,
            scope: scope(namespace),
            chunks: vec![Chunk {
                id: (0, 42, NODE),
                meta: SyncMeta {
                    parent: None,
                    parent_epoch: None,
                    marker: SyncMarker::Mutation,
                    retention_period: None,
                },
                entries,
            }],
        })
    }

    fn filter(include: &[&str], exclude: &[&str]) -> Filter {
        let subjects = |namespaces: &[&str]| {
            namespaces
                .iter()
                .map(|namespace| Subject::Namespace(String::from(*namespace)))
                .collect()
        };
        Filter {
            include: subjects(include),
            exclude: subjects(exclude),
        }
    }

    fn rendered(msg: &ReplicaMessage, filter: &Filter, detail: bool) -> Option<String> {
        render(
            SystemTime::UNIX_EPOCH,
            "a0b1c2d3e4f5",
            100,
            msg,
            filter,
            detail,
            false,
        )
    }

    fn styled(msg: &ReplicaMessage, filter: &Filter, detail: bool) -> Option<String> {
        render(
            SystemTime::UNIX_EPOCH,
            "a0b1c2d3e4f5",
            100,
            msg,
            filter,
            detail,
            true,
        )
    }

    #[test]
    fn changeset_outside_the_filter_is_hidden() {
        let msg = changeset("cells", vec![]);

        assert_eq!(rendered(&msg, &filter(&["sys"], &[]), false), None);
    }

    #[test]
    fn changeset_in_an_excluded_namespace_is_hidden() {
        let msg = changeset("cells", vec![]);

        assert_eq!(rendered(&msg, &filter(&[], &["cells"]), false), None);
        assert!(rendered(&msg, &filter(&[], &["sys"]), false).is_some());
    }

    #[test]
    fn exclusion_wins_inside_an_included_namespace() {
        let msg = changeset("cells", vec![]);
        let narrower = Filter {
            include: vec![Subject::Namespace(String::from("cells"))],
            exclude: vec![Subject::Database(String::from("cells"), String::from("db"))],
        };

        assert_eq!(rendered(&msg, &narrower, false), None);
    }

    #[test]
    fn changeset_renders_scope_marker_and_entries() {
        let msg = changeset("cells", vec![(b"k1".to_vec(), Some(b"v".to_vec()))]);

        let out = rendered(&msg, &filter(&[], &[]), false).expect("no filter admits everything");
        assert!(out.contains("CHANGESET"), "{out}");
        assert!(out.contains("cells/db/p"), "{out}");
        assert!(out.contains("mutation"), "{out}");
        assert!(out.contains("1 entries"), "{out}");
        assert!(out.contains("a0b1c2d3"), "{out}");
    }

    #[test]
    fn detail_lists_entry_keys() {
        let msg = changeset(
            "cells",
            vec![
                (b"kv/counter".to_vec(), Some(b"v".to_vec())),
                (b"kv/gone".to_vec(), None),
            ],
        );

        let out = rendered(&msg, &filter(&[], &[]), true).expect("no filter admits everything");
        assert!(out.contains("put kv/counter"), "{out}");
        assert!(out.contains("del kv/gone"), "{out}");
    }

    #[test]
    fn announce_scopes_are_filtered() {
        let mut known = VecMap::new();
        known.insert(
            scope("cells"),
            ScopeAnnounce::full(BTreeMap::from([(42, (0, NODE))])),
        );
        known.insert(
            scope("sys"),
            ScopeAnnounce::full(BTreeMap::from([(43, (1, NODE))])),
        );
        let msg = ReplicaMessage::Announce(Announce {
            known,
            full_replica: true,
        });

        let out = rendered(&msg, &filter(&["sys"], &[]), false).expect("one scope matches");
        assert!(out.contains("1 of 2 scope(s)"), "{out}");
        assert!(out.contains("sys/db/p"), "{out}");
        assert!(!out.contains("cells/db/p"), "{out}");

        assert_eq!(rendered(&msg, &filter(&["gw"], &[]), false), None);
    }

    #[test]
    fn announce_scopes_are_trimmed_by_exclusion() {
        let mut known = VecMap::new();
        known.insert(
            scope("cells"),
            ScopeAnnounce::full(BTreeMap::from([(42, (0, NODE))])),
        );
        known.insert(
            scope("sys"),
            ScopeAnnounce::full(BTreeMap::from([(43, (1, NODE))])),
        );
        let msg = ReplicaMessage::Announce(Announce {
            known,
            full_replica: true,
        });

        let out = rendered(&msg, &filter(&[], &["cells"]), false).expect("one scope survives");
        assert!(out.contains("1 of 2 scope(s)"), "{out}");
        assert!(out.contains("sys/db/p"), "{out}");
        assert!(!out.contains("cells/db/p"), "{out}");

        assert_eq!(rendered(&msg, &filter(&[], &["cells", "sys"]), false), None);
    }

    #[test]
    fn probe_for_everything_always_shows() {
        let msg = ReplicaMessage::Probe(Probe { filter: vec![] });

        let out = rendered(&msg, &filter(&["sys"], &[]), false)
            .expect("an unfiltered probe concerns everyone");
        assert!(out.contains("all scopes"), "{out}");

        let out = rendered(&msg, &filter(&[], &["sys"]), false)
            .expect("an unfiltered probe also concerns the unexcluded");
        assert!(out.contains("all scopes"), "{out}");
    }

    #[test]
    fn probe_outside_the_filter_is_hidden() {
        let msg = ReplicaMessage::Probe(Probe {
            filter: vec![scope("cells")],
        });

        assert_eq!(rendered(&msg, &filter(&["sys"], &[]), false), None);
        assert_eq!(rendered(&msg, &filter(&[], &["cells"]), false), None);
    }

    #[test]
    fn each_scope_gets_a_stable_distinct_colour() {
        assert_eq!(scope_palette("cells/db/p"), scope_palette("cells/db/p"));
        // A different scope usually lands on a different hue; these two do.
        assert_ne!(scope_palette("cells/db/p"), scope_palette("sys/db/p"));
    }

    #[test]
    fn styled_output_wraps_scope_head_count_and_size_in_ansi() {
        let msg = changeset("cells", vec![(b"k".to_vec(), Some(b"v".to_vec()))]);

        let out = styled(&msg, &filter(&[], &[]), false).expect("no filter admits everything");
        // Scope in its palette colour, size in the size colour, both reset.
        assert!(
            out.contains(&format!("{}cells/db/p{RESET}", scope_palette("cells/db/p"))),
            "{out}"
        );
        assert!(out.contains(SIZE), "{out}");
        assert!(out.contains(RESET), "{out}");

        let mut known = VecMap::new();
        known.insert(
            scope("cells"),
            ScopeAnnounce::full(BTreeMap::from([(42, (0, NODE)), (43, (1, NODE))])),
        );
        let announce = ReplicaMessage::Announce(Announce {
            known,
            full_replica: true,
        });
        let out = styled(&announce, &filter(&[], &[]), false).expect("no filter admits everything");
        assert!(out.contains(&format!("{HEADS}2{RESET} head(s)")), "{out}");
    }

    #[test]
    fn changeset_req_without_a_cursor_reads_since_the_beginning() {
        let msg = ReplicaMessage::ChangeSetReq(ChangeSetReq {
            tx_id: None,
            scope: scope("cells"),
            since_ts: None,
            epoch_floors: BTreeMap::new(),
        });

        let out = rendered(&msg, &filter(&[], &[]), false).expect("no filter admits everything");
        assert!(out.contains("since the beginning"), "{out}");
        assert!(out.contains("0 epoch floor(s)"), "{out}");
    }

    #[test]
    fn stats_count_messages_per_type_scope_and_sender() {
        let mut window = StatsWindow::new();
        let all = filter(&[], &[]);

        let msg = changeset("cells", vec![(b"k".to_vec(), Some(b"v".to_vec()))]);
        window.record("a0b1c2d3e4f5", 100, &msg, &all);
        window.record("a0b1c2d3e4f5", 300, &msg, &all);
        window.record(
            "ffff0000ffff",
            50,
            &ReplicaMessage::Probe(Probe { filter: vec![] }),
            &all,
        );

        let out = window.render_for(SystemTime::UNIX_EPOCH, Duration::from_secs(4));
        // 400 bytes over 4s from one sender's changesets.
        assert!(out.contains("CHANGESET"), "{out}");
        assert!(out.contains("cells/db/p"), "{out}");
        assert!(out.contains("a0b1c2d3"), "{out}");
        assert!(out.contains("0.5"), "{out}"); // 2 msgs / 4s
        assert!(out.contains("100 B/s"), "{out}");
        assert!(out.contains("PROBE"), "{out}");
        assert!(out.contains("TOTAL"), "{out}");
        assert!(out.contains("3 msg(s)"), "{out}");
    }

    #[test]
    fn stats_respect_the_filter() {
        let mut window = StatsWindow::new();
        let narrowed = filter(&["sys"], &[]);

        let msg = changeset("cells", vec![]);
        window.record("a0b1c2d3e4f5", 100, &msg, &narrowed);

        let out = window.render_for(SystemTime::UNIX_EPOCH, Duration::from_secs(5));
        assert!(out.contains("no replication traffic"), "{out}");
    }

    #[test]
    fn stats_reset_clears_the_window() {
        let mut window = StatsWindow::new();
        let all = filter(&[], &[]);

        window.record("a0b1c2d3e4f5", 100, &changeset("cells", vec![]), &all);
        window.reset();

        let out = window.render_for(SystemTime::UNIX_EPOCH, Duration::from_secs(5));
        assert!(out.contains("no replication traffic"), "{out}");
    }
}
