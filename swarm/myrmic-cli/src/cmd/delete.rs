use std::io::IsTerminal as _;

use anyhow::Context as _;
use cell_protocol::{CellInstance, Sri};
use dialoguer::Select;
use sorg_client::Client;

use crate::args::Ctx;

#[derive(clap::Parser)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one bool per mutually-exclusive action flag"
)]
pub struct Delete {
    /// The SRI/SRN of a cell, or the name of an app, to delete.
    #[arg(value_name = "SRI/SRN/APP")]
    target: String,
    /// Delete the whole app the target belongs to.
    #[arg(long, group = "action")]
    app: bool,
    /// Delete just the target cell.
    #[arg(long, group = "action")]
    cell: bool,
    /// Delete the target cell together with all of its descendants.
    #[arg(long, group = "action")]
    branch: bool,
    /// Delete the target cell's descendants, leaving the cell itself.
    #[arg(long, group = "action")]
    children: bool,
}

impl Delete {
    /// The action a flag pins, if any. Clap's `action` group guarantees at most
    /// one is set.
    fn flagged_action(&self) -> Option<Action> {
        if self.app {
            Some(Action::App)
        } else if self.cell {
            Some(Action::Cell)
        } else if self.branch {
            Some(Action::Branch)
        } else if self.children {
            Some(Action::Children)
        } else {
            None
        }
    }
}

/// One thing `delete` can do to the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Nothing,
    App,
    Cell,
    Branch,
    Children,
}

impl Action {
    /// The flag that pins this action non-interactively.
    fn flag(self) -> Option<&'static str> {
        match self {
            Action::Nothing => None,
            Action::App => Some("--app"),
            Action::Cell => Some("--cell"),
            Action::Branch => Some("--branch"),
            Action::Children => Some("--children"),
        }
    }
}

/// What the target actually is on the network — the answers that decide which
/// actions are worth offering.
struct Facts {
    /// The app `--app` would delete, if deleting it removes more than just the
    /// target cell (a single-cell app is folded into the cell action).
    app: Option<String>,
    /// The resolved cell, present only if it currently has a placement.
    cell: Option<Sri>,
    /// The cell's descendants (excludes the cell itself), deepest cells last.
    descendants: Vec<Sri>,
    /// Gateway route mounts the cell serves.
    routes: Vec<String>,
}

impl Facts {
    /// The actions worth offering, in menu order. `Nothing` is always first, so
    /// it is the default cursor position.
    fn actions(&self) -> Vec<Action> {
        let mut actions = vec![Action::Nothing];
        if self.app.is_some() {
            actions.push(Action::App);
        }
        if self.cell.is_some() {
            actions.push(Action::Cell);
            if !self.descendants.is_empty() {
                actions.push(Action::Branch);
                actions.push(Action::Children);
            }
        }
        actions
    }

    fn label(&self, action: Action, target: &str) -> String {
        match action {
            Action::Nothing => "Nothing".to_owned(),
            Action::App => format!(
                "Delete the application (app: {})",
                self.app.as_deref().unwrap_or(target)
            ),
            Action::Cell => {
                let sri = self.cell.expect("cell action requires a resolved cell");
                let routes = if self.routes.is_empty() {
                    String::new()
                } else {
                    format!(" — serves {} gateway route(s)", self.routes.len())
                };
                format!("Delete the cell (srn: {target}, sri: {sri}){routes}")
            }
            Action::Branch => "Delete the cell (w/ children)".to_owned(),
            Action::Children => "Delete just the children".to_owned(),
        }
    }
}

pub async fn handle(ctx: Ctx, cmd: Delete) -> anyhow::Result<()> {
    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    let facts = gather_facts(&client, &cmd.target).await?;

    let action = if let Some(flagged) = cmd.flagged_action() {
        validate_flag(flagged, &facts, &cmd.target)?;
        flagged
    } else if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        prompt_action(&facts, &cmd.target)?
    } else {
        let flags: Vec<&str> = facts.actions().iter().filter_map(|a| a.flag()).collect();
        anyhow::bail!(
            "refusing to prompt without a terminal; pass one of: {}",
            flags.join(" "),
        );
    };

    execute(ctx, &client, &cmd.target, &facts, action).await
}

/// Learns what the target is: an app, a placed cell, both, or neither.
async fn gather_facts(client: &Client, target: &str) -> anyhow::Result<Facts> {
    let sri =
        Sri::from_target(target).map_err(|e| anyhow::anyhow!("invalid target '{target}': {e}"))?;

    let placement = client.get_placement(&sri).await?;
    let cell = placement.as_ref().map(|_| sri);

    // The target either names an app directly, or resolves to a cell that
    // belongs to one.
    let members_by_target = client.app_members(target).await?;
    let app_name = if members_by_target.is_empty() {
        placement.as_ref().and_then(|p| p.app.clone())
    } else {
        Some(target.to_owned())
    };

    // Offer `--app` only when it removes more than just this one cell —
    // otherwise deleting the app is identical to deleting the cell.
    let app = match app_name {
        Some(name) => {
            let members = if name == target {
                members_by_target
            } else {
                client.app_members(&name).await?
            };
            if cell.is_some_and(|c| members.as_slice() == [c]) {
                None
            } else {
                Some(name)
            }
        }
        None => None,
    };

    let (descendants, routes) = if cell.is_some() {
        let instances = client.list_instances().await?;
        (
            descendants(&instances, sri),
            client.cell_routes(&sri).await?,
        )
    } else {
        (Vec::new(), Vec::new())
    };

    if app.is_none() && cell.is_none() {
        anyhow::bail!("nothing named '{target}' is deployed");
    }

    Ok(Facts {
        app,
        cell,
        descendants,
        routes,
    })
}

/// Every cell spawned beneath `root`, transitively, following spawn edges in
/// the instance registry. A cell is always listed before its own descendants,
/// so reversing the result yields a deepest-first (child-before-parent) order.
fn descendants(instances: &[CellInstance], root: Sri) -> Vec<Sri> {
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for inst in instances {
            if inst.lineage.parent == Some(parent) && !found.contains(&inst.sri) {
                found.push(inst.sri);
                frontier.push(inst.sri);
            }
        }
    }
    found
}

/// A flag names an action directly; reject it when the target can't support it.
fn validate_flag(action: Action, facts: &Facts, target: &str) -> anyhow::Result<()> {
    match action {
        Action::App if facts.app.is_none() => {
            anyhow::bail!("'{target}' is not an application")
        }
        Action::Cell | Action::Branch | Action::Children if facts.cell.is_none() => {
            anyhow::bail!("no deployed cell '{target}'")
        }
        Action::Children if facts.descendants.is_empty() => {
            anyhow::bail!("cell '{target}' has no children")
        }
        _ => Ok(()),
    }
}

fn prompt_action(facts: &Facts, target: &str) -> anyhow::Result<Action> {
    let actions = facts.actions();
    let labels: Vec<String> = actions.iter().map(|a| facts.label(*a, target)).collect();
    let selection = Select::new()
        .with_prompt(format!("Delete '{target}'?"))
        .items(&labels)
        .default(0)
        .interact_opt()
        .context("selection prompt failed")?;
    // Escaping the prompt is the same as choosing Nothing.
    Ok(selection.map_or(Action::Nothing, |i| actions[i]))
}

async fn execute(
    ctx: Ctx,
    client: &Client,
    target: &str,
    facts: &Facts,
    action: Action,
) -> anyhow::Result<()> {
    match action {
        Action::Nothing => {
            crate::info!(ctx, "nothing deleted");
        }
        Action::App => {
            let name = facts.app.as_deref().unwrap_or(target);
            client
                .delete_application(name)
                .await
                .context("application deletion failed")?;
            crate::info!(ctx, "deleted application '{name}'");
        }
        Action::Cell => {
            let sri = facts.cell.expect("cell action requires a resolved cell");
            client.undeploy_cell(sri).await?;
            crate::info!(ctx, "undeployed cell '{target}' (sri {sri})");
        }
        Action::Branch => {
            let sri = facts.cell.expect("branch action requires a resolved cell");
            undeploy_descendants(ctx, client, facts).await?;
            client.undeploy_cell(sri).await?;
            crate::info!(
                ctx,
                "undeployed cell '{target}' (sri {sri}) and {} descendant(s)",
                facts.descendants.len(),
            );
        }
        Action::Children => {
            undeploy_descendants(ctx, client, facts).await?;
            crate::info!(
                ctx,
                "undeployed {} child cell(s) of '{target}'",
                facts.descendants.len(),
            );
        }
    }
    Ok(())
}

/// Undeploys a cell's descendants deepest-first, so a mid-way failure never
/// leaves a live child orphaned under a deleted parent.
async fn undeploy_descendants(ctx: Ctx, client: &Client, facts: &Facts) -> anyhow::Result<()> {
    for child in facts.descendants.iter().rev() {
        client
            .undeploy_cell(*child)
            .await
            .with_context(|| format!("undeploying child cell {child}"))?;
        crate::debug!(ctx, "undeployed child cell (sri {child})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use cell_protocol::{Gen, SpawnLineage};

    use super::*;

    fn sri(name: &str) -> Sri {
        Sri::of_path(name).unwrap()
    }

    fn instance(name: &str, parent: Option<&str>) -> CellInstance {
        CellInstance {
            sri: sri(name),
            class_name: "test".to_owned(),
            gen_id: Gen::from_parts(0, 0),
            lineage: SpawnLineage {
                parent: parent.map(sri),
                ..Default::default()
            },
        }
    }

    fn facts(app: Option<&str>, cell: Option<&str>, descendants: &[&str]) -> Facts {
        Facts {
            app: app.map(str::to_owned),
            cell: cell.map(sri),
            descendants: descendants.iter().map(|n| sri(n)).collect(),
            routes: Vec::new(),
        }
    }

    #[test]
    fn descendants_walks_the_whole_subtree() {
        let instances = [
            instance("app/root", None),
            instance("app/a", Some("app/root")),
            instance("app/b", Some("app/root")),
            instance("app/a1", Some("app/a")),
            instance("other", None),
        ];
        let mut found = descendants(&instances, sri("app/root"));
        found.sort_by_key(Sri::as_uuid);
        let mut expected = vec![sri("app/a"), sri("app/b"), sri("app/a1")];
        expected.sort_by_key(Sri::as_uuid);
        assert_eq!(found, expected);
    }

    #[test]
    fn descendants_lists_a_parent_before_its_children() {
        let instances = [
            instance("app/a", Some("app/root")),
            instance("app/a1", Some("app/a")),
        ];
        let found = descendants(&instances, sri("app/root"));
        let a = found.iter().position(|s| *s == sri("app/a")).unwrap();
        let a1 = found.iter().position(|s| *s == sri("app/a1")).unwrap();
        assert!(
            a < a1,
            "parent must precede child so rev() deletes deepest-first"
        );
    }

    #[test]
    fn descendants_of_a_leaf_is_empty() {
        let instances = [
            instance("app/root", None),
            instance("app/a", Some("app/root")),
        ];
        assert!(descendants(&instances, sri("app/a")).is_empty());
    }

    #[test]
    fn menu_offers_only_applicable_actions() {
        // App + cell + children: everything.
        assert_eq!(
            facts(Some("app"), Some("app/root"), &["app/a"]).actions(),
            [
                Action::Nothing,
                Action::App,
                Action::Cell,
                Action::Branch,
                Action::Children
            ],
        );
        // Childless cell in an app: no branch/children.
        assert_eq!(
            facts(Some("app"), Some("app/root"), &[]).actions(),
            [Action::Nothing, Action::App, Action::Cell],
        );
        // Standalone childless cell: just the cell.
        assert_eq!(
            facts(None, Some("solo"), &[]).actions(),
            [Action::Nothing, Action::Cell],
        );
        // Pure app name (no placed cell): only the app.
        assert_eq!(
            facts(Some("app"), None, &[]).actions(),
            [Action::Nothing, Action::App],
        );
    }

    #[test]
    fn cell_label_annotates_gateway_routes() {
        let mut f = facts(None, Some("web"), &[]);
        f.routes = vec!["/".to_owned(), "/api".to_owned()];
        assert!(
            f.label(Action::Cell, "web")
                .contains("serves 2 gateway route(s)")
        );
    }

    #[test]
    fn validate_flag_rejects_children_without_children() {
        let f = facts(None, Some("solo"), &[]);
        assert!(validate_flag(Action::Children, &f, "solo").is_err());
        // --branch on a leaf is allowed (it just deletes the cell).
        assert!(validate_flag(Action::Branch, &f, "solo").is_ok());
    }

    #[test]
    fn validate_flag_rejects_app_on_a_plain_cell() {
        let f = facts(None, Some("solo"), &[]);
        assert!(validate_flag(Action::App, &f, "solo").is_err());
    }
}
