//! Point-in-time replication status, derived from the announces every
//! replicating node broadcasts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::time::Duration;

use anyhow::Result;
use db_client::v1::models;
use db_commons::models::ReplicaMessage;
use db_commons::models::replication::{Fingerprint, Probe};
use db_commons::topics::replica;

use crate::args::Ctx;
use crate::render::RESET;
use crate::{info, warn};

use super::monitor::{self, Filter};

const IN_SYNC: &str = "\x1b[1;32m";
const OUT_OF_SYNC: &str = "\x1b[1;31m";
const INDETERMINATE: &str = "\x1b[1;33m";

/// A snapshot of who replicates what, taken from announce traffic.
///
/// Listens for one announce round and prints, per scope, every node holding
/// it, each node's frontier, and whether the copies agree. Purely passive by
/// default; `--active` probes first so nodes answer immediately with full
/// (unelided) frontiers.
#[derive(clap::Parser)]
pub struct Status {
    /// What to inspect: `app:<name>` for a whole application, a cell's SRI (a
    /// UUID) or SRN for one cell, or `scope:ns[/db[/schema]]` for a slice of
    /// the scope hierarchy. With no identifiers, every scope is inspected.
    #[clap(value_name = "IDENTIFIER")]
    targets: Vec<String>,

    /// Hide what this identifier names; same forms as the positional
    /// identifiers, repeatable.
    #[clap(short, long, value_name = "IDENTIFIER")]
    exclude: Vec<String>,

    /// Probe for immediate full announces instead of passively waiting out an
    /// announce round. Faster, and frontiers become exactly comparable, but
    /// it asks every replicating node to broadcast its full frontier.
    #[clap(long)]
    active: bool,

    /// How long to listen for announces, in seconds [default: 10]. An
    /// --active run also stops early once answers go quiet.
    #[clap(long)]
    window: Option<u64>,
}

/// How long an `--active` collection waits after the last announce before
/// concluding the probed responders have all answered.
const ACTIVE_QUIET_GRACE: Duration = Duration::from_secs(2);

/// When the collection loop stops: at `deadline` (the window cap), or — for an
/// `--active` run that has started receiving answers — shortly after the last
/// one landed. A passive run always collects for the full window: announces
/// arrive on their own cadence, so early quiet proves nothing there.
fn collection_cutoff(
    active: bool,
    deadline: tokio::time::Instant,
    quiet: Option<tokio::time::Instant>,
) -> tokio::time::Instant {
    match quiet {
        Some(quiet) if active => quiet.min(deadline),
        _ => deadline,
    }
}

pub async fn handle(ctx: Ctx, cmd: Status) -> Result<()> {
    let selectors = monitor::parse(&cmd.targets)?;
    let excludes = monitor::parse(&cmd.exclude)?;

    let session = ctx.session().await?;
    let filter = monitor::resolve(ctx, &session, &selectors, &excludes).await?;

    if !selectors.is_empty() && filter.include.is_empty() {
        anyhow::bail!("nothing to inspect: the given identifiers name no cells");
    }

    let (subscribers, mut receiver) = monitor::subscribe_replica_topics(&session, &filter).await?;

    let window = Duration::from_secs(cmd.window.unwrap_or(10));

    if cmd.active {
        probe(&session, &filter).await?;
        info!(
            ctx,
            "probed; collecting answers for up to {} (stops {} after the last)...",
            humantime::format_duration(window),
            humantime::format_duration(ACTIVE_QUIET_GRACE),
        );
    } else {
        info!(
            ctx,
            "collecting announces for {} (--active probes for immediate full announces)...",
            humantime::format_duration(window)
        );
    }

    let deadline = tokio::time::Instant::now() + window;
    let scopes = collect_announces(ctx, &filter, &mut receiver, cmd.active, deadline).await;

    drop(subscribers);

    if scopes.is_empty() {
        info!(
            ctx,
            "no announces heard in {}: nothing replicates the inspected scopes, or no \
             node is reachable (a longer --window may help)",
            humantime::format_duration(window)
        );
        return Ok(());
    }

    let styled = std::io::stdout().is_terminal();
    print!("{}", render(&scopes, styled));

    let nodes: BTreeSet<&str> = scopes
        .values()
        .flat_map(|nodes| nodes.keys().map(String::as_str))
        .collect();
    info!(
        ctx,
        "{} scope(s) across {} node(s)",
        scopes.len(),
        nodes.len()
    );

    if !cmd.active
        && scopes
            .values()
            .any(|nodes| verdict(nodes) == Verdict::Indeterminate)
    {
        info!(
            ctx,
            "some frontiers are elided against different baselines and cannot be compared \
             exactly; rerun with --active for full frontiers"
        );
    }

    Ok(())
}

/// Collects one round of replica announces into a per-scope view, stopping at
/// the window deadline or — for an `--active` run — shortly after answers go
/// quiet.
async fn collect_announces(
    ctx: Ctx,
    filter: &Filter,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<(std::time::SystemTime, String, Vec<u8>)>,
    active: bool,
    deadline: tokio::time::Instant,
) -> Scopes {
    let mut scopes: Scopes = BTreeMap::new();
    let mut quiet: Option<tokio::time::Instant> = None;

    loop {
        let cutoff = collection_cutoff(active, deadline, quiet);
        let received = tokio::select! {
            received = receiver.recv() => received,
            () = tokio::time::sleep_until(cutoff) => break,
        };
        let Some((_, keyexpr, payload)) = received else {
            break;
        };

        let sender = match replica::parse_sender(&keyexpr) {
            Ok(id) => id.to_string(),
            Err(err) => {
                warn!(ctx, "cannot parse sender from {keyexpr}: {err}");
                continue;
            }
        };

        match postcard::from_bytes::<ReplicaMessage>(&payload) {
            Ok(ReplicaMessage::Announce(announce)) => {
                quiet = Some(tokio::time::Instant::now() + ACTIVE_QUIET_GRACE);
                for (scope, sa) in announce.known.iter() {
                    if !filter.admits(scope) {
                        continue;
                    }
                    let view = NodeView {
                        full_replica: announce.full_replica,
                        heads: sa.heads.clone(),
                        baseline: sa.baseline,
                        fingerprint: sa.fingerprint,
                    };
                    record(scopes.entry(scope.to_string()).or_default(), &sender, view);
                }
            }
            Ok(_) => {}
            Err(err) => warn!(
                ctx,
                "undecodable replica message from {sender} on {keyexpr}: {err}"
            ),
        }
    }

    scopes
}

/// Publishes an empty-filter probe on every relevant subject: each receiving
/// node answers with a full announce of everything it replicates.
async fn probe(session: &zenoh::Session, filter: &Filter) -> Result<()> {
    let payload = postcard::to_allocvec(&ReplicaMessage::Probe(Probe { filter: vec![] }))?;

    let keyexprs: BTreeSet<String> = if filter.include.is_empty() {
        BTreeSet::from([replica::format_replica("*", "*", "*", session.zid(), "*")])
    } else {
        filter
            .include
            .iter()
            .map(|subject| {
                let (namespace, database, schema) = subject.as_keyexprs();
                replica::format_replica(namespace, database, schema, session.zid(), "*")
            })
            .collect()
    };

    for keyexpr in keyexprs {
        session
            .put(&keyexpr, payload.clone())
            .await
            .map_err(|err| anyhow::anyhow!("unable to publish probe on {keyexpr}: {err}"))?;
    }

    Ok(())
}

/// Scope display name → announcing node (full hex id) → its latest frontier.
type Scopes = BTreeMap<String, BTreeMap<String, NodeView>>;

/// One node's announced frontier for one scope.
struct NodeView {
    full_replica: bool,
    heads: BTreeMap<models::Version, (models::Epoch, models::NodeId)>,
    baseline: Option<models::Version>,
    fingerprint: Fingerprint,
}

/// Keeps the most informative frontier per node: a full (unelided) one is
/// never overwritten by a floored periodic announce arriving after it.
fn record(nodes: &mut BTreeMap<String, NodeView>, node: &str, view: NodeView) {
    match nodes.entry(String::from(node)) {
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            let downgrade = entry.get().baseline.is_none() && view.baseline.is_some();
            if !downgrade {
                entry.insert(view);
            }
        }
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(view);
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Verdict {
    /// Only one node announced the scope; there is nothing to compare.
    Sole,
    /// Every announced frontier is identical.
    InSync,
    /// Frontiers share a baseline but differ.
    OutOfSync,
    /// Baselines differ, so the elided prefixes cannot be compared.
    Indeterminate,
}

fn verdict(nodes: &BTreeMap<String, NodeView>) -> Verdict {
    if nodes.len() < 2 {
        return Verdict::Sole;
    }

    let mut views = nodes.values();
    let first = views.next().expect("at least two nodes");

    let mut all_equal = true;
    let mut baselines_equal = true;
    for view in views {
        if view.baseline != first.baseline {
            baselines_equal = false;
        }
        if view.baseline != first.baseline
            || view.fingerprint != first.fingerprint
            || view.heads != first.heads
        {
            all_equal = false;
        }
    }

    if all_equal {
        Verdict::InSync
    } else if baselines_equal {
        Verdict::OutOfSync
    } else {
        Verdict::Indeterminate
    }
}

fn verdict_label(verdict: Verdict, styled: bool) -> String {
    let (color, text) = match verdict {
        Verdict::Sole => (IN_SYNC, "sole holder"),
        Verdict::InSync => (IN_SYNC, "in sync"),
        Verdict::OutOfSync => (OUT_OF_SYNC, "out of sync"),
        Verdict::Indeterminate => (INDETERMINATE, "indeterminate"),
    };
    if styled {
        format!("{color}{text}{RESET}")
    } else {
        String::from(text)
    }
}

fn render(scopes: &Scopes, styled: bool) -> String {
    let mut out = String::new();

    for (scope, nodes) in scopes {
        let verdict = verdict(nodes);

        // The scope string round-trips through Display; recolor it here.
        let label = if styled {
            monitor::scope_label(&parse_scope(scope), styled)
        } else {
            scope.clone()
        };
        let _ = writeln!(
            out,
            "{label}  {}  {} node(s)",
            verdict_label(verdict, styled),
            nodes.len()
        );

        let newest = nodes
            .values()
            .filter_map(|view| view.heads.last_key_value().map(|(ts, _)| *ts))
            .max();

        for (node, view) in nodes {
            let role = if view.full_replica {
                "full"
            } else {
                "offloader"
            };

            let frontier = match view.heads.last_key_value() {
                Some((ts, _)) => {
                    let lag = match (verdict, newest) {
                        (Verdict::OutOfSync, Some(newest)) if newest > *ts => {
                            format!("  (behind by {})", lag(newest, *ts))
                        }
                        _ => String::new(),
                    };
                    format!(
                        "{} head(s), latest {}{lag}",
                        view.heads.len(),
                        monitor::version(*ts)
                    )
                }
                None => String::from("no explicit heads"),
            };

            let baseline = match view.baseline {
                Some(baseline) => format!(
                    ", baseline {} (fingerprint {:016x})",
                    monitor::version(baseline),
                    view.fingerprint
                ),
                None => String::new(),
            };

            let _ = writeln!(
                out,
                "  {:<8}  {role:<9}  {frontier}{baseline}",
                prefix(node)
            );
        }
    }

    out
}

/// How far one HLC version trails another, as wall-clock time.
fn lag(newest: models::Version, ts: models::Version) -> impl std::fmt::Display {
    let delta = uhlc::NTP64(newest)
        .to_duration()
        .saturating_sub(uhlc::NTP64(ts).to_duration());
    humantime::format_duration(Duration::from_millis(
        u64::try_from(delta.as_millis()).unwrap_or(u64::MAX),
    ))
}

fn prefix(id: &str) -> &str {
    monitor::prefix(id)
}

/// Rebuilds a `Scope` from its `Display` form for colouring; the parts are
/// only used for the palette hash, so a lossy split is fine.
fn parse_scope(display: &str) -> models::Scope {
    let mut parts = display.splitn(3, '/');
    let namespace = parts.next().unwrap_or_default();
    let database = parts.next().unwrap_or_default();
    let schema = parts.next().unwrap_or_default();
    models::Scope::new(namespace, database, schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE: models::NodeId = [7; 16];

    fn view(
        full_replica: bool,
        heads: &[models::Version],
        baseline: Option<models::Version>,
        fingerprint: Fingerprint,
    ) -> NodeView {
        NodeView {
            full_replica,
            heads: heads.iter().map(|ts| (*ts, (0, NODE))).collect(),
            baseline,
            fingerprint,
        }
    }

    fn nodes(views: Vec<(&str, NodeView)>) -> BTreeMap<String, NodeView> {
        views
            .into_iter()
            .map(|(node, view)| (String::from(node), view))
            .collect()
    }

    #[test]
    fn one_node_is_a_sole_holder() {
        let nodes = nodes(vec![("aa", view(true, &[42], None, 0))]);
        assert_eq!(verdict(&nodes), Verdict::Sole);
    }

    #[test]
    fn identical_frontiers_are_in_sync() {
        let nodes = nodes(vec![
            ("aa", view(true, &[42, 43], Some(40), 7)),
            ("bb", view(true, &[42, 43], Some(40), 7)),
        ]);
        assert_eq!(verdict(&nodes), Verdict::InSync);
    }

    #[test]
    fn differing_heads_on_one_baseline_are_out_of_sync() {
        let nodes = nodes(vec![
            ("aa", view(true, &[42, 43], None, 0)),
            ("bb", view(true, &[42], None, 0)),
        ]);
        assert_eq!(verdict(&nodes), Verdict::OutOfSync);
    }

    #[test]
    fn differing_baselines_are_indeterminate() {
        let nodes = nodes(vec![
            ("aa", view(true, &[43], Some(40), 7)),
            ("bb", view(true, &[43], Some(41), 9)),
        ]);
        assert_eq!(verdict(&nodes), Verdict::Indeterminate);
    }

    #[test]
    fn a_full_frontier_survives_a_later_floored_announce() {
        let mut map = nodes(vec![("aa", view(true, &[42, 43], None, 0))]);

        record(&mut map, "aa", view(true, &[43], Some(42), 7));

        let kept = map.get("aa").expect("node stays");
        assert_eq!(kept.baseline, None);
        assert_eq!(kept.heads.len(), 2);
    }

    #[test]
    fn active_collection_stops_on_quiescence() {
        let now = tokio::time::Instant::now();
        let deadline = now + Duration::from_secs(10);
        let quiet = now + Duration::from_secs(2);

        // Passive runs its full window regardless of announce arrivals.
        assert_eq!(collection_cutoff(false, deadline, Some(quiet)), deadline);
        // Active with no announces yet waits out the full window.
        assert_eq!(collection_cutoff(true, deadline, None), deadline);
        // Active stops shortly after the last announce landed...
        assert_eq!(collection_cutoff(true, deadline, Some(quiet)), quiet);
        // ...but never runs past the window.
        let late = now + Duration::from_secs(30);
        assert_eq!(collection_cutoff(true, deadline, Some(late)), deadline);
    }

    #[test]
    fn a_floored_frontier_is_replaced_by_a_full_one() {
        let mut map = nodes(vec![("aa", view(true, &[43], Some(42), 7))]);

        record(&mut map, "aa", view(true, &[42, 43], None, 0));

        let kept = map.get("aa").expect("node stays");
        assert_eq!(kept.baseline, None);
        assert_eq!(kept.heads.len(), 2);
    }

    #[test]
    fn render_shows_roles_verdict_and_lag() {
        let ntp = |secs: u64| uhlc::NTP64::from(Duration::from_secs(secs)).as_u64();
        let scopes = Scopes::from([(
            String::from("cells/db/p"),
            nodes(vec![
                ("aabbccddeeff0011", view(true, &[ntp(2), ntp(4)], None, 0)),
                ("2233445566778899", view(false, &[ntp(2)], None, 0)),
            ]),
        )]);

        let out = render(&scopes, false);
        assert!(out.contains("cells/db/p"), "{out}");
        assert!(out.contains("out of sync"), "{out}");
        assert!(out.contains("aabbccdd"), "{out}");
        assert!(out.contains("full"), "{out}");
        assert!(out.contains("22334455"), "{out}");
        assert!(out.contains("offloader"), "{out}");
        assert!(out.contains("behind by 2s"), "{out}");
    }

    #[test]
    fn render_marks_agreeing_replicas_in_sync() {
        let scopes = Scopes::from([(
            String::from("cells/db/p"),
            nodes(vec![
                ("aa", view(true, &[42], Some(40), 7)),
                ("bb", view(true, &[42], Some(40), 7)),
            ]),
        )]);

        let out = render(&scopes, false);
        assert!(out.contains("in sync"), "{out}");
        assert!(out.contains("baseline"), "{out}");
        assert!(out.contains("0000000000000007"), "{out}");
    }
}
