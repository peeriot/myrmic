use std::fmt::Write as _;
use std::io::IsTerminal as _;

use anyhow::Context;

use cell_protocol::{ExecRuntimeInfo, RuntimeId};
use introspection_client::v1::{NodeStatus, ParticipantInfo};

use crate::args::Ctx;
use crate::render::{NONE, cell, styled_id, unique_prefix_lengths, width};

/// Caps for the self-reported columns. Every value in them comes from another
/// node, so one long — or hostile — string must not be able to reshape the
/// table for every other row.
const NAME_CHARS: usize = 48;
const KIND_CHARS: usize = 16;
const TAGS_CHARS: usize = 64;

#[derive(clap::Parser, Default)]
pub struct Status {}

pub async fn handle(ctx: Ctx, _cmd: Status) -> anyhow::Result<()> {
    let session = ctx.session().await?;
    let client = ctx.introspection(session.clone()).await;

    let (statuses, runtimes) = tokio::join!(
        client.swarm_status(),
        sorg_common::exec_registry::list_registered_execs(&session),
    );

    let statuses = statuses.context("unable to query network status")?;
    let runtimes = runtimes.context("unable to query registered runtimes")?;

    let nodes = join_nodes(&statuses, runtimes);

    let styled = std::io::stdout().is_terminal();
    print!("{}", render(&nodes, styled));

    Ok(())
}

/// A node on the network, with its exec registry entry when it has one, and
/// its self-description when it gave one (a CLI invocation, say).
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

    // Named runtimes first, then self-described participants, each
    // alphabetically; everything else by id. Cached, so each key is built once
    // rather than on both sides of every comparison.
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

    nodes
}

fn render(nodes: &[NodeDetails], styled: bool) -> String {
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
        let _ = writeln!(out, "{}", line.trim_end());
    }

    out
}

/// A table row, with the id pre-rendered and its visible width kept alongside
/// so the styled escapes do not throw the padding off.
struct Row {
    idx: String,
    name: String,
    kind: String,
    id: String,
    id_width: usize,
    tags: String,
}

#[cfg(test)]
mod tests {
    use cell_protocol::{CapabilityTag, ExecutionCapabilities};
    use zenoh::config::ZenohId;

    use super::*;
    use crate::render::{BOLD_CYAN, DIMMED, RESET};

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
            render(&join_nodes(&[], vec![]), false),
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
            render(&nodes, false),
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
            render(&nodes, false),
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

        assert!(render(&nodes, false).contains("m network status  cli"));
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
        let out = render(&nodes, false);

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
        let out = render(&nodes, false);

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

        assert_eq!(render(&peer, false), render(&router, false));
    }

    /// A runtime in the registry that nothing on the network links to still
    /// gets a row.
    #[test]
    fn keeps_registry_entries_with_no_reported_node() {
        let a = id("aabb112233445566");
        let b = id("bbcc112233445566");

        let nodes = join_nodes(&[status(a, &[], &[])], vec![exec(b, "offline", &[])]);

        assert_eq!(nodes.len(), 2);
        assert!(render(&nodes, false).contains("offline"));
    }

    /// Ids that share more than [`ID_CHARS`] characters widen the column until
    /// the highlighted prefix tells them apart.
    #[test]
    fn widens_the_id_column_for_long_shared_prefixes() {
        let a = id("aabbccddeeff0011");
        let b = id("aabbccddeeff0022");

        let nodes = join_nodes(&[status(a, &[b], &[])], vec![]);

        assert_eq!(
            render(&nodes, false),
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

        let out = render(&join_nodes(&[status(a, &[b], &[])], vec![]), true);

        assert!(out.contains(&format!("{BOLD_CYAN}a{RESET}{DIMMED}abb1122{RESET}")));
    }
}
