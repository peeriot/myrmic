//! Tests of [`test_framework::swarm2`] on the local backend. They need a built `myrmic` binary (see
//! [`test_framework::resolve_binary!`]).

use cell_protocol::replication::runtime_tag;
use enum_map::Enum;
use test_framework::{
    myrmic::{self, Error, Myrmic, cell::CellSpec},
    runtime::{Kill, Running, Stopped},
    swarm2::{Local, Swarm, SwarmSpec},
};
use zenoh::config::ZenohId;

/// three runtimes without tags
#[derive(Clone, Copy, PartialEq, Eq, Debug, Enum)]
enum Node {
    A,
    B,
    C,
}

impl SwarmSpec for Node {
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
    let (swarm, _runtimes) = Swarm::<_, Node>::start(Local::new()).await.unwrap();
    let cell = new_cell(&swarm.client()).await;

    for (node, entry) in swarm.get_entries() {
        let zid = entry.get_zid();
        let placed_on = deploy_pinned(&swarm.client(), &cell, zid).await;
        assert_eq!(placed_on, zid.to_string(), "the cell pinned to {node:?}");
    }
}

/// Two swarms started side by side don't see each other: each one's start checks that exactly
/// its own runtimes answer, and neither can place a cell on the other's runtime.
#[tokio::test(flavor = "multi_thread")]
async fn parallel_swarms_are_isolated() {
    let (one, other) = tokio::join!(
        Swarm::<_, Node>::start(Local::new()),
        Swarm::<_, Node>::start(Local::new())
    );
    let (one, _one_runtimes) = one.unwrap();
    let (other, _other_runtimes) = other.unwrap();
    let cell = new_cell(&one.client()).await;

    let foreign = runtime_tag(other.get_entry(Node::A).get_zid().into());
    let deployed = one
        .client()
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
    let (swarm, [_a, b, _c]) = Swarm::<_, Node>::start(Local::new()).await.unwrap();
    let zid = b.get_zid();

    let b = b.kill().await.unwrap();
    assert!(!swarm.get_entry(Node::B).is_running());
    let b = b.start().await.unwrap();
    assert_eq!(b.get_zid(), zid);

    let cell = new_cell(&swarm.client()).await;
    let placed_on = deploy_pinned(&swarm.client(), &cell, zid).await;
    assert_eq!(placed_on, zid.to_string());
}

/// a fresh cell from `myrmic new`, against the SDK in this checkout: the git default pins the
/// revision the CLI was built from, which only exists once pushed to the myrmic remote
async fn new_cell(myrmic: &Myrmic<myrmic::Local>) -> CellSpec<myrmic::Local> {
    let name = format!("Cell{}", uuid::Uuid::new_v4().simple());
    let sdk = concat!(env!("CARGO_MANIFEST_DIR"), "/../../sdk/myrmic-sdk");
    myrmic.new_cell(&name, Some(sdk)).await.unwrap()
}

/// Deploy `cell` pinned to the runtime `zid` by its runtime tag; returns the runtime `myrmic
/// deploy` reports it was placed on. The deployed cell is deleted again on return.
async fn deploy_pinned(
    myrmic: &Myrmic<myrmic::Local>,
    cell: &CellSpec<myrmic::Local>,
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
