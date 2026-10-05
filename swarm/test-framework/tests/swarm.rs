//! Tests of [`test_framework::swarm2`] on the local backend. They need a built `myrmic` binary (see
//! [`test_framework::resolve_binary!`]).

use cell_protocol::replication::runtime_tag;
use test_framework::myrmic::{Error, LocalBinary, Myrmic, cell::CellSpec};
use test_framework::swarm2::{Local, Swarm, SwarmSpec};
use zenoh::config::ZenohId;

/// three runtimes without tags
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Node {
    A,
    B,
    C,
}

impl SwarmSpec for Node {
    fn all() -> Vec<Self> {
        vec![Node::A, Node::B, Node::C]
    }

    fn tags(self) -> Vec<String> {
        vec![]
    }

    fn persistent(self) -> bool {
        false
    }
}

/// The swarm is really connected: the orchestrator places a cell on each of its runtimes.
#[tokio::test(flavor = "multi_thread")]
async fn cells_are_placed_on_every_runtime() {
    let swarm: Swarm<_, Node> = Swarm::start(Local::new()).await.unwrap();
    let cell = new_cell(swarm.myrmic()).await;

    for node in Node::all() {
        let zid = swarm.runtime(node).zid();
        let placed_on = deploy_pinned(swarm.myrmic(), &cell, zid).await;
        assert_eq!(placed_on, zid.to_string(), "the cell pinned to {node:?}");
    }
}

/// Two swarms started side by side don't see each other: each one's start checks that exactly
/// its own runtimes answer, and neither can place a cell on the other's runtime.
#[tokio::test(flavor = "multi_thread")]
async fn parallel_swarms_are_isolated() {
    let (one, other) = tokio::join!(Swarm::start(Local::new()), Swarm::start(Local::new()));
    let one: Swarm<_, Node> = one.unwrap();
    let other: Swarm<_, Node> = other.unwrap();
    let cell = new_cell(one.myrmic()).await;

    let foreign = runtime_tag(other.runtime(Node::A).zid().into());
    let deployed = one
        .myrmic()
        .cell(cell.as_path())
        .tags(&[&foreign])
        .deploy()
        .await;
    let stderr = match deployed {
        Err(Error::Cli { stderr }) => stderr,
        Err(err) => panic!("the deploy failed without the CLI failing: {err}"),
        Ok(cell) => panic!(
            "a cell pinned to the other swarm's runtime was deployed: {}",
            cell.output()
        ),
    };
    assert!(
        stderr.contains(&format!("missing required tags [{foreign}]")),
        "the deploy failed for another reason than the missing runtime: {stderr}"
    );
}

/// A killed runtime comes back under the same zid, and cells can be placed on it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_killed_runtime_restarts_under_the_same_zid() {
    let mut swarm: Swarm<_, Node> = Swarm::start(Local::new()).await.unwrap();
    let zid = swarm.runtime(Node::B).zid();

    swarm.runtime_mut(Node::B).kill().await.unwrap();
    assert!(!swarm.runtime(Node::B).is_running());
    swarm.runtime_mut(Node::B).restart().await.unwrap();
    assert_eq!(swarm.runtime(Node::B).zid(), zid);

    let cell = new_cell(swarm.myrmic()).await;
    let placed_on = deploy_pinned(swarm.myrmic(), &cell, zid).await;
    assert_eq!(placed_on, zid.to_string());
}

/// a fresh cell from `myrmic new`
async fn new_cell(myrmic: &Myrmic<LocalBinary>) -> CellSpec<LocalBinary> {
    let name = format!("Cell{}", uuid::Uuid::new_v4().simple());
    myrmic.new_cell(&name, None).await.unwrap()
}

/// Deploy `cell` pinned to the runtime `zid` by its runtime tag; returns the runtime `myrmic
/// deploy` reports it was placed on. The deployed cell is deleted again on return.
async fn deploy_pinned(
    myrmic: &Myrmic<LocalBinary>,
    cell: &CellSpec<LocalBinary>,
    zid: ZenohId,
) -> String {
    let tag = runtime_tag(zid.into());
    let deployed = myrmic
        .cell(cell.as_path())
        .tags(&[&tag])
        .deploy()
        .await
        .unwrap();
    let output = deployed.output();
    output
        .lines()
        .find_map(|line| {
            line.strip_prefix("INFO  ")?
                .strip_prefix("deployed cell (srn = ")?
                .split_once(", runtime = ")?
                .1
                .strip_suffix(')')
        })
        .unwrap_or_else(|| panic!("`myrmic deploy` reported no runtime: {output}"))
        .to_owned()
}
