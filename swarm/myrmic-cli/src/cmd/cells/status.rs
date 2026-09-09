use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use cell_protocol::{CellInstance, Gen, PlacementEntry, PlacementKind, Sri};
use sorg_common::{RestartType, root_restart};

use crate::args::Ctx;
use crate::live::{self, Phase, Pops};
use crate::models::RestartTypeName;
use crate::render::{BOLD, DIMMED, NONE, RESET, styled_id, unique_prefix_lengths, width};

#[cfg(test)]
mod tests;

#[derive(clap::Parser)]
pub struct Status {
    /// SRIs or SRNs to show; each match is rendered with its whole spawn
    /// subtree. If omitted, lists all registered cells.
    #[clap(value_name = "SRI/SRN")]
    targets: Vec<String>,

    #[clap(flatten)]
    live: live::Opts,
}

pub async fn handle(ctx: Ctx, cmd: Status) -> Result<()> {
    let targets = cmd
        .targets
        .iter()
        .map(|t| {
            Sri::from_target(t)
                .map(|sri| (t.clone(), sri))
                .map_err(|e| anyhow::anyhow!("invalid target '{t}': {e}"))
        })
        .collect::<Result<Vec<_>>>()?;

    let session = ctx.session().await?;
    let view = View {
        client: ctx.sorg(session),
        listing: Listing::new(targets),
    };
    live::run(cmd.live, view).await
}

struct View {
    client: sorg_client::Client,
    listing: Listing,
}

type Snapshot = (
    Vec<PlacementEntry>,
    Vec<CellInstance>,
    HashMap<Sri, RestartType>,
);

impl live::View for View {
    type Snapshot = Snapshot;

    async fn fetch(&self) -> Result<Snapshot> {
        let (cells, instances, restarts) = tokio::join!(
            self.client.list_placements(),
            self.client.list_instances(),
            root_restart::list_specs(self.client.session()),
        );
        let cells = cells?;
        let instances = instances?;
        // Only the trigger is rendered, so the full deployment record does not
        // travel into the render path.
        let policies = restarts?
            .into_iter()
            .map(|d| (d.sri, d.restart.restart_type))
            .collect();

        Ok((cells, instances, policies))
    }

    fn apply(&mut self, snapshot: Snapshot, now: Instant) {
        self.listing.apply(snapshot, now);
    }

    fn draw(&self, now: Instant, styled: bool) -> String {
        self.listing.draw(now, styled)
    }

    fn settle(&mut self) {
        self.listing.settle();
    }
}

/// The cells being shown: the last snapshot, plus cells that have since gone
/// and are still fading out. A respawn changes the generation, so it pops
/// like an arrival while the row stays where it is.
struct Listing {
    targets: Vec<(String, Sri)>,
    rows: HashMap<Sri, (PlacementEntry, Option<CellInstance>)>,
    /// The live root-restart specs, one entry per restartable root. Held on
    /// the listing rather than passed straight through so a row held for the
    /// fade can keep the spec it departed with.
    policies: HashMap<Sri, RestartType>,
    pops: Pops<Sri, Gen>,
}

impl Listing {
    fn new(targets: Vec<(String, Sri)>) -> Self {
        Self {
            targets,
            rows: HashMap::new(),
            policies: HashMap::new(),
            pops: Pops::new(live::FADE),
        }
    }

    fn apply(&mut self, (cells, instances, mut policies): Snapshot, now: Instant) {
        self.pops
            .observe(now, cells.iter().map(|c| (c.sri, c.gen_id)));

        let mut by_sri: HashMap<Sri, CellInstance> =
            instances.into_iter().map(|i| (i.sri, i)).collect();
        let mut rows: HashMap<Sri, _> = cells
            .into_iter()
            .map(|entry| {
                let instance = by_sri.remove(&entry.sri);
                (entry.sri, (entry, instance))
            })
            .collect();
        for sri in self.pops.departing(now) {
            if let Some(row) = self.rows.remove(sri) {
                rows.insert(*sri, row);
            }
            if let Some(&policy) = self.policies.get(sri) {
                policies.entry(*sri).or_insert(policy);
            }
        }
        self.rows = rows;
        self.policies = policies;
    }

    fn draw(&self, now: Instant, styled: bool) -> String {
        let (cells, instances): (Vec<_>, Vec<_>) = self
            .rows
            .values()
            .filter(|(entry, _)| self.pops.phase(&entry.sri, now) != Phase::Gone)
            .map(|(entry, instance)| (entry.clone(), instance.clone()))
            .unzip();
        let instances = instances.into_iter().flatten().collect();
        let highlight = |sri: &Sri| styled.then(|| self.pops.phase(sri, now).sgr()).flatten();
        render(
            cells,
            instances,
            &self.policies,
            &self.targets,
            styled,
            SystemTime::now(),
            &highlight,
        )
    }

    fn settle(&mut self) {
        self.pops.settle();
        self.rows.retain(|sri, _| self.pops.present(sri));
    }
}

/// A registered cell joined with its instance row (the lineage source), plus
/// its spawn children.
struct Node {
    entry: PlacementEntry,
    instance: Option<CellInstance>,
    name: Option<String>,
    policy: &'static str,
    children: Vec<usize>,
}

struct Forest {
    nodes: Vec<Node>,
    roots: Vec<usize>,
    index: HashMap<Sri, usize>,
}

/// The cell's own SRN segment: the spawn-time local name, or for roots the
/// class name when it derives back to the SRI (how the CLI names deploys).
/// SRIs are one-way hashes, so a name that survived neither way is gone.
fn name_segment(entry: &PlacementEntry, instance: Option<&CellInstance>) -> Option<String> {
    let instance = instance?;
    if let Some(name) = &instance.lineage.local_name {
        return Some(name.clone());
    }
    if instance.lineage.parent.is_none()
        && Sri::of_path(&instance.class_name).is_ok_and(|sri| sri == entry.sri)
    {
        return Some(instance.class_name.clone());
    }
    None
}

/// The restart policy in force for this cell, as `--policy` spells it.
///
/// A live spec wins whatever the placement kind is - only enabled root specs
/// are stored - so a root back to a placeholder mid-restart still reports its
/// policy. Absent one a placed wasm root is `never`; anything else has none.
fn policy_label(
    entry: &PlacementEntry,
    instance: Option<&CellInstance>,
    policies: &HashMap<Sri, RestartType>,
) -> &'static str {
    if let Some(&restart_type) = policies.get(&entry.sri) {
        return RestartTypeName::spelling(restart_type);
    }

    let is_root = instance.is_some_and(|i| i.lineage.parent.is_none());
    if is_root && matches!(entry.kind, PlacementKind::Wasm { .. }) {
        return RestartTypeName::spelling(RestartType::Never);
    }

    NONE
}

impl Forest {
    fn build(
        cells: Vec<PlacementEntry>,
        instances: Vec<CellInstance>,
        policies: &HashMap<Sri, RestartType>,
    ) -> Self {
        let mut by_sri: HashMap<Sri, CellInstance> =
            instances.into_iter().map(|i| (i.sri, i)).collect();

        let mut nodes: Vec<Node> = cells
            .into_iter()
            .map(|entry| {
                let instance = by_sri.remove(&entry.sri);
                Node {
                    name: name_segment(&entry, instance.as_ref()),
                    policy: policy_label(&entry, instance.as_ref(), policies),
                    entry,
                    instance,
                    children: vec![],
                }
            })
            .collect();

        let index: HashMap<Sri, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.entry.sri, i))
            .collect();

        let mut roots = vec![];
        for i in 0..nodes.len() {
            let parent = nodes[i]
                .instance
                .as_ref()
                .and_then(|inst| inst.lineage.parent);
            match parent.and_then(|p| index.get(&p).copied()) {
                Some(p) if p != i => nodes[p].children.push(i),
                _ => roots.push(i),
            }
        }

        // Named first, alphabetically; unnamed by sri. Same order everywhere.
        let key = |i: usize, nodes: &[Node]| {
            let n = &nodes[i];
            (
                n.name.is_none(),
                n.name.clone().unwrap_or_default(),
                n.entry.sri.to_string(),
            )
        };
        roots.sort_by_key(|&i| key(i, &nodes));
        for i in 0..nodes.len() {
            let mut children = std::mem::take(&mut nodes[i].children);
            children.sort_by_key(|&c| key(c, &nodes));
            nodes[i].children = children;
        }

        Self {
            nodes,
            roots,
            index,
        }
    }

    /// The full SRN, walked up the spawn edges. A chain that cannot reach a
    /// named root keeps its known tail behind `…/`.
    fn srn(&self, idx: usize) -> String {
        let mut segments: Vec<&str> = vec![];
        let mut complete = false;
        let mut cur = idx;
        for _ in 0..=self.nodes.len() {
            let node = &self.nodes[cur];
            let Some(name) = &node.name else { break };
            segments.push(name);
            let Some(instance) = &node.instance else {
                break;
            };
            match instance.lineage.parent.and_then(|p| self.index.get(&p)) {
                None if instance.lineage.parent.is_none() => {
                    complete = true;
                    break;
                }
                None => break,
                Some(&parent) => cur = parent,
            }
        }
        segments.reverse();
        if complete {
            segments.join("/")
        } else if segments.is_empty() {
            NONE.to_owned()
        } else {
            format!("…/{}", segments.join("/"))
        }
    }

    /// Emits `(tree prefix, node)` rows for a subtree, depth-first. The
    /// visited set keeps corrupt parent cycles from recursing forever.
    fn push_subtree(
        &self,
        idx: usize,
        prefix: String,
        child_prefix: &str,
        rows: &mut Vec<(String, usize)>,
        visited: &mut HashSet<usize>,
    ) {
        if !visited.insert(idx) {
            return;
        }
        rows.push((prefix, idx));
        let last = self.nodes[idx].children.len().saturating_sub(1);
        for (i, &child) in self.nodes[idx].children.iter().enumerate() {
            let (glyph, cont) = if i == last {
                ("└─ ", "   ")
            } else {
                ("├─ ", "│  ")
            };
            self.push_subtree(
                child,
                format!("{child_prefix}{glyph}"),
                &format!("{child_prefix}{cont}"),
                rows,
                visited,
            );
        }
    }
}

type Group = (Option<String>, Vec<(String, usize)>);

/// Places a top-level tree into its app's group; spawned cells inherit the
/// parent's app at deploy time, so the root's app covers the whole tree.
fn place_tree(forest: &Forest, idx: usize, groups: &mut Vec<Group>, visited: &mut HashSet<usize>) {
    if visited.contains(&idx) {
        return;
    }
    let app = forest.nodes[idx].entry.app.clone();
    let group = match groups.iter().position(|(a, _)| *a == app) {
        Some(pos) => pos,
        None => {
            groups.push((app, vec![]));
            groups.len() - 1
        }
    };
    forest.push_subtree(idx, String::new(), "", &mut groups[group].1, visited);
}

/// `highlight` gives a row's colour when it is fading in or out.
fn render(
    cells: Vec<PlacementEntry>,
    instances: Vec<CellInstance>,
    policies: &HashMap<Sri, RestartType>,
    targets: &[(String, Sri)],
    styled: bool,
    now: SystemTime,
    highlight: &dyn Fn(&Sri) -> Option<&'static str>,
) -> String {
    let forest = Forest::build(cells, instances, policies);
    let mut out = String::new();
    let mut groups: Vec<Group> = vec![];

    if targets.is_empty() {
        if forest.nodes.is_empty() {
            return "No cells registered\n".to_owned();
        }
        let mut visited = HashSet::new();
        for &root in &forest.roots {
            place_tree(&forest, root, &mut groups, &mut visited);
        }
        // Nodes stranded by a parent cycle still get a top-level row.
        for idx in 0..forest.nodes.len() {
            place_tree(&forest, idx, &mut groups, &mut visited);
        }
        groups.sort_by_key(|(app, _)| (app.is_none(), app.clone().unwrap_or_default()));
    } else {
        // Targets are explicit subtrees in the order given; no app sections.
        let mut placed: Vec<(String, usize)> = vec![];
        for (raw, sri) in targets {
            match forest.index.get(sri) {
                Some(&idx) => {
                    forest.push_subtree(idx, String::new(), "", &mut placed, &mut HashSet::new());
                }
                None => {
                    let _ = writeln!(out, "Cell {raw} is not registered");
                }
            }
        }
        if !placed.is_empty() {
            groups.push((None, placed));
        }
    }

    if !groups.is_empty() {
        table(
            &forest,
            &groups,
            targets.is_empty(),
            styled,
            now,
            highlight,
            &mut out,
        );
    }
    out
}

struct Row {
    key: Sri,
    cell: String,
    sri: String,
    kind: &'static str,
    runtime: Option<String>,
    age: String,
    policy: &'static str,
    class: String,
    srn: String,
}

/// How long ago the current incarnation was placed, from its generation (an
/// HLC timestamp minted at deploy admission). A respawn mints a fresh
/// generation, so this resets while the sri and runtime stay put — the visible
/// signal that a cell came back. `—` when the generation can't be read.
fn incarnation_age(gen_id: &Gen, now: SystemTime) -> String {
    let Some(ts) = gen_id.to_timestamp() else {
        return NONE.to_owned();
    };
    let placed = SystemTime::UNIX_EPOCH + ts.get_time().to_duration();
    fmt_age(now.duration_since(placed).unwrap_or_default())
}

/// The largest whole time unit of `age`, e.g. `8s`, `3m`, `2h`, `4d`.
fn fmt_age(age: Duration) -> String {
    let s = age.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3_600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3_600)
    } else {
        format!("{}d", s / 86_400)
    }
}

fn table(
    forest: &Forest,
    groups: &[Group],
    sectioned: bool,
    styled: bool,
    now: SystemTime,
    highlight: &dyn Fn(&Sri) -> Option<&'static str>,
    out: &mut String,
) {
    let sections: Vec<(Option<&str>, Vec<Row>)> = groups
        .iter()
        .map(|(app, placed)| {
            let rows = placed
                .iter()
                .map(|(prefix, idx)| {
                    let node = &forest.nodes[*idx];
                    Row {
                        key: node.entry.sri,
                        cell: format!("{prefix}{}", node.name.as_deref().unwrap_or(NONE)),
                        sri: node.entry.sri.to_string(),
                        kind: match &node.entry.kind {
                            PlacementKind::Wasm { .. } => "wasm",
                            PlacementKind::Native { .. } => "native",
                            PlacementKind::Bridge { .. } => "bridge",
                            PlacementKind::Placeholder => "N/A",
                        },
                        runtime: node.entry.kind.host().map(|id| id.to_string()),
                        age: incarnation_age(&node.entry.gen_id, now),
                        policy: node.policy,
                        class: node
                            .instance
                            .as_ref()
                            .map_or_else(|| NONE.to_owned(), |i| i.class_name.clone()),
                        srn: forest.srn(*idx),
                    }
                })
                .collect();
            (app.as_deref(), rows)
        })
        .collect();
    let rows = || sections.iter().flat_map(|(_, rows)| rows);

    // Prefix uniqueness is computed over the distinct runtime ids shown, so
    // repeats do not widen the column to the full id.
    let mut distinct: Vec<String> = vec![];
    for id in rows().filter_map(|r| r.runtime.as_deref()) {
        if !distinct.iter().any(|d| d == id) {
            distinct.push(id.to_owned());
        }
    }
    let uniq = unique_prefix_lengths(&distinct);
    let rendered: HashMap<&str, (String, usize)> = distinct
        .iter()
        .zip(&uniq)
        .map(|(id, &n)| (id.as_str(), styled_id(id, n, styled)))
        .collect();

    let cw = width(rows().map(|r| r.cell.as_str()).chain(["cell"]));
    let sw = width(rows().map(|r| r.sri.as_str()).chain(["sri"]));
    let kw = width(rows().map(|r| r.kind).chain(["kind"]));
    let rw = rendered
        .values()
        .map(|(_, w)| *w)
        .max()
        .unwrap_or(0)
        .max("runtime".len());
    let aw = width(rows().map(|r| r.age.as_str()).chain(["age"]));
    let pw = width(rows().map(|r| r.policy).chain(["policy"]));
    let clw = width(rows().map(|r| r.class.as_str()).chain(["class"]));
    let srn_w = width(rows().map(|r| r.srn.as_str()).chain(["srn"]));
    let total = 2 + cw + 2 + sw + 2 + kw + 2 + rw + 2 + aw + 2 + pw + 2 + clw + 2 + srn_w;

    let _ = writeln!(
        out,
        "  {:cw$}  {:sw$}  {:kw$}  {:rw$}  {:aw$}  {:pw$}  {:clw$}  srn",
        "cell", "sri", "kind", "runtime", "age", "policy", "class",
    );

    for (app, rows) in &sections {
        if sectioned {
            let _ = writeln!(out, "{}", rule(*app, total, styled));
        }
        for row in rows {
            let (id, id_width) = row
                .runtime
                .as_deref()
                .map_or((NONE.to_owned(), 1), |id| rendered[id].clone());
            let pad = " ".repeat(rw - id_width);
            let line = format!(
                "  {cell:cw$}  {sri:sw$}  {kind:kw$}  {id}{pad}  {age:aw$}  {policy:pw$}  {class:clw$}  {srn}",
                cell = row.cell,
                sri = row.sri,
                kind = row.kind,
                age = row.age,
                policy = row.policy,
                class = row.class,
                srn = row.srn,
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
    }
}

/// A full-width section rule — `──── app ────…` for an app, an unbroken line
/// for the ungrouped section — filled out to the table width so the tree rows
/// keep their indentation.
fn rule(app: Option<&str>, total: usize, styled: bool) -> String {
    let Some(app) = app else {
        let line = "─".repeat(total);
        return if styled {
            format!("{DIMMED}{line}{RESET}")
        } else {
            line
        };
    };
    let tail = "─".repeat(total.saturating_sub(app.chars().count() + 6));
    if styled {
        format!("{DIMMED}────{RESET} {BOLD}{app}{RESET} {DIMMED}{tail}{RESET}")
    } else {
        format!("──── {app} {tail}")
    }
}
