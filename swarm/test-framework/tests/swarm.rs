//! Tests of [`test_framework::swarm3`] with CLI runtimes on the host. They need a built `myrmic`
//! binary (see [`test_framework::resolve_binary!`]).

use cell_protocol::replication::runtime_tag;
use introspection_client::v1::Client;
use test_framework::{
    World, host, in_process,
    myrmic::{self, Error, Myrmic, cell::CellSpec},
    overlay::Overlay,
    runtime::{Running, Stopped},
    start_runtimes,
    swarm3::{RunningRt, Swarm},
    wait::DEFAULT_TIMEOUT,
    wait_for,
};
use zenoh::config::ZenohId;

/// a running CLI runtime of a swarm
type CliRt = RunningRt<myrmic::RunningRt<myrmic::Local>>;

/// a swarm of three CLI runtimes on one group, started
async fn three_runtimes() -> Result<(Swarm, [CliRt; 3]), Error> {
    let group = Overlay::new(&World::local()).new_multicast_group();
    let swarm = Swarm::new(&group).await;
    let a = swarm.add(swarm.get_myrmic().runtime());
    let b = swarm.add(swarm.get_myrmic().runtime());
    let c = swarm.add(swarm.get_myrmic().runtime());
    start_runtimes!(swarm; a, b, c);
    Ok((swarm, [a, b, c]))
}

/// The swarm is really connected: the orchestrator places a cell on each of its runtimes.
#[tokio::test(flavor = "multi_thread")]
async fn cells_are_placed_on_every_runtime() -> anyhow::Result<()> {
    let (swarm, runtimes) = three_runtimes().await?;
    let cell = new_cell(swarm.get_myrmic()).await?;

    for runtime in &runtimes {
        let zid = runtime.get_zid();
        let placed_on = deploy_pinned(swarm.get_myrmic(), &cell, zid).await?;
        assert_eq!(placed_on, zid.to_string(), "the cell pinned to {zid}");
    }
    Ok(())
}

/// Two swarms started side by side don't see each other: each one's start checks that exactly
/// its own runtimes answer, and neither can place a cell on the other's runtime.
#[tokio::test(flavor = "multi_thread")]
async fn parallel_swarms_are_isolated() -> anyhow::Result<()> {
    let (one, other) = tokio::join!(three_runtimes(), three_runtimes());
    let (one, _one_runtimes) = one?;
    let (_other, [other_a, _other_b, _other_c]) = other?;
    let cell = new_cell(one.get_myrmic()).await?;

    let foreign = runtime_tag(other_a.get_zid().into());
    let deployed = one
        .get_myrmic()
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
    Ok(())
}

/// A killed runtime comes back under the same zid, and cells can be placed on it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_killed_runtime_restarts_under_the_same_zid() -> anyhow::Result<()> {
    let (swarm, [_a, b, _c]) = three_runtimes().await?;
    let zid = b.get_zid();

    let b = b.stop_with(|rt| rt.kill()).await?;
    let b = b.start().await?;
    assert_eq!(b.get_zid(), zid);

    let cell = new_cell(swarm.get_myrmic()).await?;
    let placed_on = deploy_pinned(swarm.get_myrmic(), &cell, zid).await?;
    assert_eq!(placed_on, zid.to_string());
    Ok(())
}

/// A runtime moved to a group of its own comes back cut off: the swarm's runtimes no longer
/// reach it, and it reaches none of them.
#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_moved_to_its_own_group_is_cut_off() -> anyhow::Result<()> {
    let overlay = Overlay::new(&World::local());
    let group = overlay.new_multicast_group();
    let swarm = Swarm::new(&group).await;
    let a = swarm.add(swarm.get_myrmic().runtime());
    let b = swarm.add(swarm.get_myrmic().runtime());
    start_runtimes!(swarm; a, b);

    let b = b.stop_with(|rt| rt.kill()).await?;
    let alone = overlay.new_multicast_group();
    overlay.set_group(&b, &alone);
    let b = b.start_without_check().await?;

    // `a` answers alone, linked to nobody: `b` is expected to stay silent
    swarm.wait_until_connected().await?;
    let lone = Client::new(zenoh::open(alone.get_zenoh_config()).await.unwrap()).await;
    let status = wait_for(DEFAULT_TIMEOUT, "the cut-off runtime to answer", || async {
        lone.swarm_status()
            .await
            .expect("failed to query the status on the lone group")
            .into_iter()
            .find(|status| status.id == b.get_zid())
    })
    .await?;
    assert!(
        status.peers.is_empty() && status.routers.is_empty(),
        "the cut-off runtime is linked to {:?} and {:?}",
        status.peers,
        status.routers
    );
    Ok(())
}

/// A runtime attached by a connect links to its target: the target lists it as its peer.
#[tokio::test(flavor = "multi_thread")]
async fn a_connected_runtime_links_to_its_target() -> anyhow::Result<()> {
    let overlay = Overlay::new(&World::local());
    let group = overlay.new_multicast_group();
    let target = in_process::StoppedRt::new(&group).orchestration();
    let to_target = overlay.connect_to(&target);
    let target = target.start().await?;
    let connected = host::Local
        .myrmic_isolated(to_target)
        .runtime()
        .start()
        .await?;

    let client = Client::new(target.get_session().clone()).await;
    wait_for(
        DEFAULT_TIMEOUT,
        "the target to list the connected runtime",
        || async {
            let status = client
                .own_status()
                .await
                .expect("failed to query the target's own status");
            status.peers.contains(&connected.get_zid()).then_some(())
        },
    )
    .await?;
    Ok(())
}

/// A runtime that connects to one member of a group ends up linked to all of the group: zenoh's
/// gossip tells it about its target's links, and it connects to them. The swarm's check expects
/// exactly that, so its start only returns when the links are there.
#[tokio::test(flavor = "multi_thread")]
async fn a_connected_runtime_links_to_its_targets_whole_group() -> anyhow::Result<()> {
    let overlay = Overlay::new(&World::local());
    let group = overlay.new_multicast_group();
    let swarm = Swarm::new(&group).await;
    let target = in_process::StoppedRt::new(&group).orchestration();
    let to_target = overlay.connect_to(&target);
    let target = swarm.add(target);
    let other = swarm.add(in_process::StoppedRt::new(&group).orchestration());
    let connected = swarm.add(in_process::StoppedRt::new(to_target).orchestration());
    start_runtimes!(swarm; target, other, connected);
    Ok(())
}

/// a fresh cell from `myrmic new`, against the SDK in this checkout: the git default pins the
/// revision the CLI was built from, which only exists once pushed to the myrmic remote
async fn new_cell(myrmic: &Myrmic<myrmic::Local>) -> Result<CellSpec<myrmic::Local>, Error> {
    let name = format!("Cell{}", uuid::Uuid::new_v4().simple());
    let sdk = concat!(env!("CARGO_MANIFEST_DIR"), "/../../sdk/myrmic-sdk");
    myrmic.new_cell(&name, Some(sdk)).await
}

/// Deploy `cell` pinned to the runtime `zid` by its runtime tag; returns the runtime `myrmic
/// deploy` reports it was placed on. The deployed cell is deleted again on return.
async fn deploy_pinned(
    myrmic: &Myrmic<myrmic::Local>,
    cell: &CellSpec<myrmic::Local>,
    zid: ZenohId,
) -> Result<String, Error> {
    let tag = runtime_tag(zid.into());
    let deployed = myrmic.cell(cell.as_path()).tags(&[&tag]).deploy().await?;
    let output = deployed.output();
    Ok(output
        .lines()
        .find_map(|line| {
            line.strip_prefix("INFO  ")?
                .strip_prefix("deployed cell (srn = ")?
                .split_once(", runtime = ")?
                .1
                .strip_suffix(')')
        })
        .unwrap_or_else(|| panic!("`myrmic deploy` reported no runtime: {output}"))
        .to_owned())
}
