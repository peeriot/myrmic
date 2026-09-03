use std::fmt::Write as _;
use std::time::Instant;

use anyhow::Context;

use cell_protocol::{ExecRuntimeInfo, RuntimeId};
use introspection_client::v1::{NodeStatus, ParticipantInfo};

use crate::args::Ctx;
use crate::live::{self, Phase, Pops};
use crate::render::{NONE, cell, styled_id, unique_prefix_lengths, width};

/// Caps for the self-reported columns. Every value in them comes from another
/// node, so one long — or hostile — string must not be able to reshape the
/// table for every other row.
const NAME_CHARS: usize = 48;
const KIND_CHARS: usize = 16;
const TAGS_CHARS: usize = 128;

#[derive(clap::Parser)]
pub struct Status {
    #[clap(flatten)]
    live: live::Opts,
}

pub async fn handle(ctx: Ctx, cmd: Status) -> anyhow::Result<()> {
    let session = ctx.session().await?;
    let view = View {
        client: ctx.introspection(session.clone()).await,
        session,
        listing: Listing::new(),
    };
    live::run(cmd.live, view).await
}

struct View {
    session: zenoh::Session,
    client: introspection_client::v1::Client,
    listing: Listing,
}

impl live::View for View {
    type Snapshot = Vec<NodeDetails>;

    async fn fetch(&self) -> anyhow::Result<Vec<NodeDetails>> {
        let (statuses, runtimes) = tokio::join!(
            self.client.swarm_status(),
            sorg_common::exec_registry::list_registered_execs(&self.session),
        );
        let statuses = statuses.context("unable to query network status")?;
        let runtimes = runtimes.context("unable to query registered runtimes")?;
        Ok(join_nodes(&statuses, runtimes))
    }

    fn apply(&mut self, nodes: Vec<NodeDetails>, now: Instant) {
        self.listing.apply(nodes, now);
    }

    fn draw(&self, now: Instant, styled: bool) -> String {
        self.listing.draw(now, styled)
    }

    fn settle(&mut self) {
        self.listing.settle();
    }
}

/// The nodes being shown: the last snapshot, plus nodes that have since gone
/// and are still fading out.
struct Listing {
    nodes: Vec<NodeDetails>,
    pops: Pops<RuntimeId>,
}

impl Listing {
    fn new() -> Self {
        Self {
            nodes: vec![],
            pops: Pops::new(live::FADE),
        }
    }

    fn apply(&mut self, mut nodes: Vec<NodeDetails>, now: Instant) {
        self.pops.observe(now, nodes.iter().map(|n| (n.id, ())));
        for id in self.pops.departing(now) {
            if let Some(node) = self.nodes.iter().find(|n| n.id == *id) {
                nodes.push(node.clone());
            }
        }
        sort_nodes(&mut nodes);
        self.nodes = nodes;
    }

    fn draw(&self, now: Instant, styled: bool) -> String {
        let shown: Vec<&NodeDetails> = self
            .nodes
            .iter()
            .filter(|n| self.pops.phase(&n.id, now) != Phase::Gone)
            .collect();
        let highlight = |id: &RuntimeId| styled.then(|| self.pops.phase(id, now).sgr()).flatten();
        render(&shown, styled, &highlight)
    }

    fn settle(&mut self) {
        self.pops.settle();
        self.nodes.retain(|n| self.pops.present(&n.id));
    }
}

/// A node on the network, with its exec registry entry when it has one, and
/// its self-description when it gave one (a CLI invocation, say).
#[derive(Clone)]
struct NodeDetails {
    id: RuntimeId,
    exec: Option<ExecRuntimeInfo>,
    participant: Option<ParticipantInfo>,
}

impl NodeDetails {
    /// The registry name, else the participant's self-description together with
    /// where it runs — `m db monitor @ jezza@spin`. A participant means little
    /// without its origin: half the network may be running `m db monitor`.
    fn name(&self) -> String {
        if let Some(name) = self.exec.as_ref().and_then(ExecRuntimeInfo::name) {
            return name.to_owned();
        }

        let Some(participant) = &self.participant else {
            return NONE.to_owned();
        };

        match &participant.origin {
            Some(origin) => format!("{} @ {origin}", participant.name),
            None => participant.name.clone(),
        }
    }

    fn kind(&self) -> String {
        if let Some(exec) = &self.exec {
            return exec.runtime_kind().to_string();
        }
        self.participant
            .as_ref()
            .map_or_else(|| NONE.to_string(), |p| p.kind.clone())
    }

    fn tags(&self) -> String {
        self.exec.as_ref().map_or_else(String::new, |exec| {
            exec.capabilities()
                .tags()
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>()
                .join(", ")
        })
    }
}

/// Every id seen on the network — nodes that reported a status of their own,
/// plus every id they link to — joined against the exec registry. Peers and
/// routers are treated alike; both are just another node on the far end of a
/// link. A registry entry with no matching node is kept, so a runtime that
/// registered but is not on the network still shows up.
fn join_nodes(statuses: &[NodeStatus], runtimes: Vec<ExecRuntimeInfo>) -> Vec<NodeDetails> {
    let mut nodes: Vec<NodeDetails> = vec![];

    for status in statuses {
        // The reporting node itself, carrying whatever it said about itself. It
        // may already have a row from another node's links, so attach rather
        // than assume; only its own status ever sets a participant.
        let own = RuntimeId::from(status.id);
        match nodes.iter_mut().find(|n| n.id == own) {
            Some(node) if status.participant.is_some() => {
                node.participant.clone_from(&status.participant);
            }
            Some(_) => {}
            None => nodes.push(NodeDetails {
                id: own,
                exec: None,
                participant: status.participant.clone(),
            }),
        }

        for id in status.peers.iter().chain(&status.routers) {
            let id = RuntimeId::from(*id);
            if !nodes.iter().any(|n| n.id == id) {
                nodes.push(NodeDetails {
                    id,
                    exec: None,
                    participant: None,
                });
            }
        }
    }

    for exec in runtimes {
        let id = exec.id();
        match nodes.iter_mut().find(|n| n.id == id) {
            Some(node) => node.exec = Some(exec),
            None => nodes.push(NodeDetails {
                id,
                exec: Some(exec),
                participant: None,
            }),
        }
    }

    sort_nodes(&mut nodes);
    nodes
}

/// Named runtimes first, then self-described participants, each
/// alphabetically; everything else by id. Cached, so each key is built once
/// rather than on both sides of every comparison.
fn sort_nodes(nodes: &mut [NodeDetails]) {
    nodes.sort_by_cached_key(|n| {
        let name = n.exec.as_ref().and_then(ExecRuntimeInfo::name);
        let participant = n.participant.as_ref().map(|p| p.name.as_str());
        (
            name.is_none(),
            name.unwrap_or_default().to_owned(),
            participant.is_none(),
            participant.unwrap_or_default().to_owned(),
            n.id.to_string(),
        )
    });
}

/// `highlight` gives a row's colour when it is fading in or out.
fn render(
    nodes: &[&NodeDetails],
    styled: bool,
    highlight: &dyn Fn(&RuntimeId) -> Option<&'static str>,
) -> String {
    if nodes.is_empty() {
        return "No nodes reported. Is an introspection plugin running on the network?\n"
            .to_string();
    }

    let ids: Vec<String> = nodes.iter().map(|n| n.id.to_string()).collect();
    let uniq = unique_prefix_lengths(&ids);

    let rows: Vec<Row> = nodes
        .iter()
        .zip(&ids)
        .zip(&uniq)
        .enumerate()
        .map(|(i, ((node, id), &uniq_len))| {
            let (id, id_width) = styled_id(id, uniq_len, styled);
            Row {
                key: node.id,
                idx: i.to_string(),
                name: cell(&node.name(), NAME_CHARS),
                kind: cell(&node.kind(), KIND_CHARS),
                id,
                id_width,
                tags: cell(&node.tags(), TAGS_CHARS),
            }
        })
        .collect();

    let iw = rows.iter().map(|r| r.idx.len()).max().unwrap_or(1);
    let nw = width(rows.iter().map(|r| r.name.as_str()).chain(["name"]));
    let kw = width(rows.iter().map(|r| r.kind.as_str()).chain(["kind"]));
    let dw = rows.iter().map(|r| r.id_width).max().unwrap_or(2).max(2);

    let mut out = String::new();
    let _ = writeln!(out, "Discovered {} node(s)\n", rows.len());
    let _ = writeln!(
        out,
        "  {:>iw$}  {:nw$}  {:kw$}  {:dw$}  tags",
        "#", "name", "kind", "id",
    );

    for row in &rows {
        let pad = " ".repeat(dw - row.id_width);
        let line = format!(
            "  {idx:>iw$}  {name:nw$}  {kind:kw$}  {id}{pad}  {tags}",
            idx = row.idx,
            name = row.name,
            kind = row.kind,
            id = row.id,
            tags = row.tags,
        );
        let line = line.trim_end();
        match highlight(&row.key) {
            Some(sgr) => {
                let _ = writeln!(out, "{}", live::paint(line, sgr));
            }
            None => {
                let _ = writeln!(out, "{line}");
            }
        }
    }

    out
}

/// A table row, with the id pre-rendered and its visible width kept alongside
/// so the styled escapes do not throw the padding off.
struct Row {
    key: RuntimeId,
    idx: String,
    name: String,
    kind: String,
    id: String,
    id_width: usize,
    tags: String,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cell_protocol::{CapabilityTag, ExecutionCapabilities};
    use zenoh::config::ZenohId;

    use super::*;
    use crate::render::{BOLD_CYAN, DIMMED, RESET};

    /// Renders with no row fading in or out.
    fn show(nodes: &[NodeDetails], styled: bool) -> String {
        render(&nodes.iter().collect::<Vec<_>>(), styled, &|_| None)
    }

    /// Builds an id that displays as `hex`; zenoh renders an id's bytes
    /// little-endian, so the leading byte has to be non-zero.
    fn id(hex: &str) -> ZenohId {
        let mut bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        bytes.reverse();
        ZenohId::try_from(&bytes[..]).unwrap()
    }

    fn exec(id: ZenohId, name: &str, tags: &[&str]) -> ExecRuntimeInfo {
        let tags = tags.iter().map(|t| CapabilityTag::new(*t)).collect();
        ExecRuntimeInfo::new(id, Some(name.to_owned()), ExecutionCapabilities::new(tags))
    }

    fn status(id: ZenohId, peers: &[ZenohId], routers: &[ZenohId]) -> NodeStatus {
        NodeStatus {
            id,
            participant: None,
            peers: peers.to_vec(),
            routers: routers.to_vec(),
            plugins: vec![],
        }
    }

    fn cli(id: ZenohId, name: &str, origin: Option<&str>) -> NodeStatus {
        NodeStatus {
            participant: Some(ParticipantInfo {
                kind: "cli".to_owned(),
                name: name.to_owned(),
                origin: origin.map(str::to_owned),
            }),
            ..status(id, &[], &[])
        }
    }

    #[test]
    fn renders_nothing_when_the_network_is_empty() {
        assert_eq!(
            show(&join_nodes(&[], vec![]), false),
            "No nodes reported. Is an introspection plugin running on the network?\n"
        );
    }

    #[test]
    fn joins_exec_details_onto_reported_nodes() {
        let a = id("aabb112233445566");
        let b = id("bbcc112233445566");
        let c = id("ccdd112233445566");

        let nodes = join_nodes(
            &[status(a, &[b], &[c])],
            vec![
                exec(b, "esp-kitchen", &["esp32c6"]),
                exec(a, "default", &["linux", "wasm"]),
            ],
        );

        assert_eq!(
            show(&nodes, false),
            "\
Discovered 3 node(s)

  #  name         kind     id          tags
  0  default      linux    [a]abb1122  linux, wasm
  1  esp-kitchen  esp32c6  [b]bcc1122  esp32c6
  2  —            —        [c]cdd1122
"
        );
    }

    /// A participant that described itself (here: a CLI invocation) renders
    /// with its own name and kind, its origin folded into the name. `tags`
    /// stays what it says it is — capability tags — and a participant has none.
    #[test]
    fn labels_self_described_participants() {
        let a = id("aabb112233445566");
        let c = id("ccdd112233445566");

        let nodes = join_nodes(
            &[
                status(a, &[c], &[]),
                cli(c, "m db monitor", Some("jezza@spin")),
            ],
            vec![exec(a, "default", &["linux"])],
        );

        assert_eq!(
            show(&nodes, false),
            "\
Discovered 2 node(s)

  #  name                       kind   id          tags
  0  default                    linux  [a]abb1122  linux
  1  m db monitor @ jezza@spin  cli    [c]cdd1122
"
        );
    }

    /// A participant with no discoverable origin is just its name.
    #[test]
    fn omits_the_origin_when_a_participant_has_none() {
        let c = id("ccdd112233445566");

        let nodes = join_nodes(&[cli(c, "m network status", None)], vec![]);

        assert!(show(&nodes, false).contains("m network status  cli"));
    }

    /// Participant strings come from whoever is on the network. An escape
    /// sequence must not reach the terminal and a newline must not break the
    /// table into pieces.
    #[test]
    fn strips_control_characters_from_reported_names() {
        let c = id("ccdd112233445566");

        let nodes = join_nodes(
            &[cli(c, "m db monitor\n\x1b[2Jwiped", Some("jezza@spin"))],
            vec![],
        );
        let out = show(&nodes, false);

        assert!(!out.contains('\x1b'));
        assert!(out.contains("m db monitor[2Jwiped @ jezza@spin"));
        // count, blank, header, one row — the newline did not split the row
        assert_eq!(out.lines().count(), 4);
    }

    /// One over-long name is truncated rather than widening the column for
    /// every other row.
    #[test]
    fn truncates_an_over_long_reported_name() {
        let a = id("aabb112233445566");
        let c = id("ccdd112233445566");

        let nodes = join_nodes(
            &[status(a, &[c], &[]), cli(c, &"x".repeat(200), None)],
            vec![exec(a, "default", &["linux"])],
        );
        let out = show(&nodes, false);

        assert!(out.contains(&format!("{}…", "x".repeat(NAME_CHARS - 1))));
        for line in out.lines().skip(2) {
            assert!(line.chars().count() < 100, "row too wide: {line}");
        }
    }

    /// A node's peers and routers are the same thing here: both are just
    /// another id on the network, and neither reported a status of its own.
    #[test]
    fn treats_peers_and_routers_alike() {
        let a = id("aabb112233445566");
        let b = id("bbcc112233445566");
        let c = id("ccdd112233445566");

        let peer = join_nodes(&[status(a, &[b, c], &[])], vec![]);
        let router = join_nodes(&[status(a, &[], &[b, c])], vec![]);

        assert_eq!(show(&peer, false), show(&router, false));
    }

    /// A runtime in the registry that nothing on the network links to still
    /// gets a row.
    #[test]
    fn keeps_registry_entries_with_no_reported_node() {
        let a = id("aabb112233445566");
        let b = id("bbcc112233445566");

        let nodes = join_nodes(&[status(a, &[], &[])], vec![exec(b, "offline", &[])]);

        assert_eq!(nodes.len(), 2);
        assert!(show(&nodes, false).contains("offline"));
    }

    /// Ids that share more than [`ID_CHARS`] characters widen the column until
    /// the highlighted prefix tells them apart.
    #[test]
    fn widens_the_id_column_for_long_shared_prefixes() {
        let a = id("aabbccddeeff0011");
        let b = id("aabbccddeeff0022");

        let nodes = join_nodes(&[status(a, &[b], &[])], vec![]);

        assert_eq!(
            show(&nodes, false),
            "\
Discovered 2 node(s)

  #  name  kind  id                 tags
  0  —     —     [aabbccddeeff001]
  1  —     —     [aabbccddeeff002]
"
        );
    }

    #[test]
    fn highlights_the_unique_prefix_when_styled() {
        let a = id("aabb112233445566");
        let b = id("bbcc112233445566");

        let out = show(&join_nodes(&[status(a, &[b], &[])], vec![]), true);

        assert!(out.contains(&format!("{BOLD_CYAN}a{RESET}{DIMMED}abb1122{RESET}")));
    }

    /// A fading row is coloured end to end: the id column's own reset must
    /// not return the rest of the row to normal.
    #[test]
    fn paints_highlighted_rows_end_to_end() {
        let a = id("aabb112233445566");
        let c = id("ccdd112233445566");
        let nodes = join_nodes(
            &[status(a, &[c], &[])],
            vec![exec(a, "default", &["linux"])],
        );
        let fading = RuntimeId::from(c);

        let out = render(&nodes.iter().collect::<Vec<_>>(), true, &|id| {
            (*id == fading).then_some("<g>")
        });

        // count, blank, header, then the rows
        let rows: Vec<&str> = out.lines().skip(3).collect();
        assert!(!rows[0].contains("<g>"), "{:?}", rows[0]);
        assert!(rows[1].starts_with("<g>"), "{:?}", rows[1]);
        assert!(rows[1].ends_with(RESET), "{:?}", rows[1]);
        assert!(rows[1].contains(&format!("{RESET}<g>")), "{:?}", rows[1]);
    }

    /// A node that joins fades in, a node that leaves fades out struck
    /// through and is dropped once the fade is over — or straight away when
    /// settling for the final frame.
    #[test]
    fn arrivals_and_departures_fade_through_the_listing() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let a = id("aabb112233445566");
        let b = id("bbcc112233445566");
        let alone = || join_nodes(&[status(a, &[], &[])], vec![exec(a, "default", &[])]);
        let both = || {
            join_nodes(
                &[status(a, &[b], &[])],
                vec![exec(a, "default", &[]), exec(b, "second", &[])],
            )
        };
        let row = |out: &str, name: &str| {
            out.lines()
                .find(|l| l.contains(name))
                .unwrap_or_else(|| panic!("no row for {name} in {out:?}"))
                .to_owned()
        };
        let arriving = Phase::Arriving(0).sgr().unwrap();
        let departing = Phase::Departing(0).sgr().unwrap();

        let mut listing = Listing::new();
        listing.apply(alone(), at(0));
        assert!(row(&listing.draw(at(0), true), "default").starts_with("  "));

        listing.apply(both(), at(100));
        let out = listing.draw(at(100), true);
        assert!(row(&out, "second").starts_with(arriving));
        assert!(row(&out, "default").starts_with("  "));
        assert!(out.starts_with("Discovered 2 node(s)"));

        listing.apply(alone(), at(200));
        let out = listing.draw(at(200), true);
        assert!(row(&out, "second").starts_with(departing));
        assert!(out.starts_with("Discovered 2 node(s)"));
        let out = listing.draw(at(200) + live::FADE, true);
        assert!(!out.contains("second"));
        assert!(out.starts_with("Discovered 1 node(s)"));

        listing.apply(alone(), at(300));
        listing.settle();
        assert!(!listing.draw(at(300), true).contains("second"));
    }

    /// `m network` is `m network status`, so the status arguments are
    /// accepted directly.
    #[test]
    fn bare_network_takes_the_status_arguments() {
        use clap::Parser as _;

        use crate::args::{Args, Command};
        use crate::cmd::network::{Cmd, Network};

        let args = Args::try_parse_from(["myrmic", "network", "--once"]).unwrap();
        let Command::Network(Network { cmd: None, status }) = args.command else {
            panic!("expected bare network");
        };
        assert!(status.live.once);

        let args =
            Args::try_parse_from(["myrmic", "nodes", "status", "--interval", "500ms"]).unwrap();
        let Command::Network(Network {
            cmd: Some(Cmd::Status(status)),
            ..
        }) = args.command
        else {
            panic!("expected network status");
        };
        assert!(!status.live.once);
        assert_eq!(
            Duration::from(status.live.interval),
            Duration::from_millis(500)
        );
    }
}
