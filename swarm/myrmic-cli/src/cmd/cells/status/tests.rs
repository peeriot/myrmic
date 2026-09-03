use cell_protocol::{
    ExecRuntimeInfo, ExecutionCapabilities, Gen, PlacementKind, SpawnLineage, Sri,
};
use zenoh::config::ZenohId;

use super::*;
use crate::render::{BOLD, BOLD_CYAN, DIMMED, RESET};

/// Builds an id that displays as `hex`; zenoh renders an id's bytes
/// little-endian, so the leading byte has to be non-zero.
fn runtime_id(hex: &str) -> ZenohId {
    let mut bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    bytes.reverse();
    ZenohId::try_from(&bytes[..]).unwrap()
}

fn exec(id: ZenohId, name: &str) -> ExecRuntimeInfo {
    ExecRuntimeInfo::new(
        id,
        Some(name.to_owned()),
        ExecutionCapabilities::new(vec![]),
    )
}

fn wasm(sri: Sri, runtime: &ExecRuntimeInfo) -> PlacementEntry {
    PlacementEntry {
        sri,
        kind: PlacementKind::Wasm {
            runtime: runtime.clone(),
        },
        app: None,
        gen_id: Gen::from_parts(1, 1),
    }
}

fn instance(sri: Sri, class: &str, parent: Option<Sri>, local_name: Option<&str>) -> CellInstance {
    CellInstance {
        sri,
        class_name: class.to_owned(),
        gen_id: Gen::from_parts(1, 1),
        lineage: SpawnLineage {
            parent,
            parent_gen_id: parent.map(|_| Gen::from_parts(1, 1)),
            local_name: local_name.map(str::to_owned),
            ..SpawnLineage::default()
        },
    }
}

fn sri(path: &str) -> Sri {
    Sri::of_path(path).unwrap()
}

/// Renders with no row fading in or out.
fn show(
    cells: Vec<PlacementEntry>,
    instances: Vec<CellInstance>,
    targets: &[(String, Sri)],
    styled: bool,
    now: SystemTime,
) -> String {
    render(cells, instances, targets, styled, now, &|_| None)
}

/// A respawn mints a fresh generation, so the age column resets while the
/// sri and runtime stay put — the whole point of the column.
#[test]
fn age_column_tracks_the_current_incarnation() {
    // A generation minted `secs` after the unix epoch: NTP64 packs whole
    // seconds in the high 32 bits.
    fn gen_at(secs: u64) -> Gen {
        Gen::from_parts(secs << 32, 1)
    }
    let rt = exec(runtime_id("bbcc112233445566"), "edge");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let snapshot = |gen_id: Gen| {
        let mut entry = wasm(sri("sensor"), &rt);
        entry.gen_id = gen_id;
        show(
            vec![entry],
            vec![instance(sri("sensor"), "sensor", None, None)],
            &[],
            false,
            now,
        )
    };

    // First incarnation, placed 900s ago.
    let before = snapshot(gen_at(100));
    assert!(before.contains("15m"), "{before}");

    // Respawned onto the same runtime 5s ago: runtime unchanged, age reset.
    let after = snapshot(gen_at(995));
    assert!(after.contains("bcc1122"), "{after}");
    assert!(after.contains("5s"), "{after}");
    assert!(!after.contains("15m"), "{after}");
}

/// Two roots (`my-app` with a spawn tree under it, `counter`) on two
/// runtimes, plus a bridge with no instance row. Input order is scrambled
/// to prove the render sorts.
fn tree_fixture() -> (Vec<PlacementEntry>, Vec<CellInstance>) {
    let rt_a = exec(runtime_id("aabb112233445566"), "dev");
    let rt_b = exec(runtime_id("bbcc112233445566"), "edge");

    let cells = vec![
        wasm(sri("my-app/worker"), &rt_a),
        PlacementEntry {
            sri: sri("bridge"),
            kind: PlacementKind::Bridge {
                sri: sri("bridge-mb"),
            },
            app: None,
            gen_id: Gen::from_parts(1, 1),
        },
        wasm(sri("my-app/gateway/session-1"), &rt_b),
        wasm(sri("my-app"), &rt_a),
        wasm(sri("counter"), &rt_b),
        wasm(sri("my-app/gateway"), &rt_a),
    ];
    let instances = vec![
        instance(sri("my-app"), "my-app", None, None),
        instance(
            sri("my-app/gateway"),
            "gateway",
            Some(sri("my-app")),
            Some("gateway"),
        ),
        instance(
            sri("my-app/gateway/session-1"),
            "session",
            Some(sri("my-app/gateway")),
            Some("session-1"),
        ),
        instance(
            sri("my-app/worker"),
            "worker",
            Some(sri("my-app")),
            Some("worker"),
        ),
        instance(sri("counter"), "counter", None, None),
    ];

    (cells, instances)
}

#[test]
fn lists_all_cells_as_a_spawn_tree() {
    let (cells, instances) = tree_fixture();

    let expected = format!(
        "  cell             sri                                   kind    runtime     age  class    srn
{rule}
  counter          {counter}  wasm    [b]bcc1122  0s   counter  counter
  my-app           {my_app}  wasm    [a]abb1122  0s   my-app   my-app
  ├─ gateway       {gateway}  wasm    [a]abb1122  0s   gateway  my-app/gateway
  │  └─ session-1  {session}  wasm    [b]bcc1122  0s   session  my-app/gateway/session-1
  └─ worker        {worker}  wasm    [a]abb1122  0s   worker   my-app/worker
  —                {bridge}  bridge  —           0s   —        —
",
        rule = "─".repeat(115),
        counter = sri("counter"),
        my_app = sri("my-app"),
        gateway = sri("my-app/gateway"),
        session = sri("my-app/gateway/session-1"),
        worker = sri("my-app/worker"),
        bridge = sri("bridge"),
    );

    assert_eq!(
        show(cells, instances, &[], false, SystemTime::UNIX_EPOCH),
        expected
    );
}

/// A chain that cannot be walked to a named root renders with a `…/`
/// prefix; a root whose name is unrecoverable gets no srn at all.
fn partial_fixture() -> (Vec<PlacementEntry>, Vec<CellInstance>, Sri) {
    let rt_a = exec(runtime_id("aabb112233445566"), "dev");
    let anon = Sri::from_uuid(uuid::Uuid::from_u128(0x42));

    let cells = vec![wasm(sri("ghost/orphan"), &rt_a), wasm(anon, &rt_a)];
    let instances = vec![
        instance(
            sri("ghost/orphan"),
            "orphan-class",
            Some(sri("ghost")),
            Some("orphan"),
        ),
        instance(anon, "mycell", None, None),
    ];

    (cells, instances, anon)
}

#[test]
fn marks_unreconstructable_srn_prefixes() {
    let (cells, instances, anon) = partial_fixture();

    let expected = format!(
        "  cell    sri                                   kind  runtime     age  class         srn
{rule}
  orphan  {orphan}  wasm  [a]abb1122  0s   orphan-class  …/orphan
  —       {anon}  wasm  [a]abb1122  0s   mycell        —
",
        rule = "─".repeat(93),
        orphan = sri("ghost/orphan"),
    );

    assert_eq!(
        show(cells, instances, &[], false, SystemTime::UNIX_EPOCH),
        expected
    );
}

#[test]
fn filters_to_the_targets_subtrees() {
    let (cells, instances) = tree_fixture();
    let targets = vec![
        ("gateway".to_owned(), sri("my-app/gateway")),
        ("worker".to_owned(), sri("my-app/worker")),
    ];

    let expected = format!(
        "  cell          sri                                   kind  runtime     age  class    srn
  gateway       {gateway}  wasm  [a]abb1122  0s   gateway  my-app/gateway
  └─ session-1  {session}  wasm  [b]bcc1122  0s   session  my-app/gateway/session-1
  worker        {worker}  wasm  [a]abb1122  0s   worker   my-app/worker
",
        gateway = sri("my-app/gateway"),
        session = sri("my-app/gateway/session-1"),
        worker = sri("my-app/worker"),
    );

    assert_eq!(
        show(cells, instances, &targets, false, SystemTime::UNIX_EPOCH),
        expected
    );
}

#[test]
fn reports_unregistered_targets() {
    let (cells, instances) = tree_fixture();
    let targets = vec![
        ("counter".to_owned(), sri("counter")),
        ("nope".to_owned(), sri("nope")),
    ];

    let expected = format!(
        "\
Cell nope is not registered
  cell     sri                                   kind  runtime     age  class    srn
  counter  {counter}  wasm  [b]bcc1122  0s   counter  counter
",
        counter = sri("counter"),
    );

    assert_eq!(
        show(cells, instances, &targets, false, SystemTime::UNIX_EPOCH),
        expected
    );
}

#[test]
fn reports_an_empty_registry() {
    assert_eq!(
        show(vec![], vec![], &[], false, SystemTime::UNIX_EPOCH),
        "No cells registered\n"
    );
}

#[test]
fn highlights_the_unique_runtime_prefix_when_styled() {
    let (cells, instances) = tree_fixture();

    let out = show(cells, instances, &[], true, SystemTime::UNIX_EPOCH);

    assert!(out.contains(&format!("{BOLD_CYAN}a{RESET}{DIMMED}abb1122{RESET}")));
}

/// Trees under two apps (`beta` as a wasm tree, `alpha` as a bridge) plus
/// an ungrouped cell.
fn apped_fixture() -> (Vec<PlacementEntry>, Vec<CellInstance>) {
    let rt_a = exec(runtime_id("aabb112233445566"), "dev");

    let mut beta_root = wasm(sri("beta-app"), &rt_a);
    beta_root.app = Some("beta".to_owned());
    let mut beta_worker = wasm(sri("beta-app/worker"), &rt_a);
    beta_worker.app = Some("beta".to_owned());

    let cells = vec![
        wasm(sri("counter"), &rt_a),
        beta_worker,
        PlacementEntry {
            sri: sri("site"),
            kind: PlacementKind::Bridge {
                sri: sri("site-mb"),
            },
            app: Some("alpha".to_owned()),
            gen_id: Gen::from_parts(1, 1),
        },
        beta_root,
    ];
    let instances = vec![
        instance(sri("beta-app"), "beta-app", None, None),
        instance(
            sri("beta-app/worker"),
            "worker",
            Some(sri("beta-app")),
            Some("worker"),
        ),
        instance(sri("counter"), "counter", None, None),
    ];

    (cells, instances)
}

#[test]
fn groups_trees_into_app_sections() {
    let (cells, instances) = apped_fixture();

    let expected = format!(
        "  cell       sri                                   kind    runtime     age  class     srn
──── alpha {rule_alpha}
  —          {site}  bridge  —           0s   —         —
──── beta {rule_beta}
  beta-app   {beta}  wasm    [a]abb1122  0s   beta-app  beta-app
  └─ worker  {worker}  wasm    [a]abb1122  0s   worker    beta-app/worker
{rule_none}
  counter    {counter}  wasm    [a]abb1122  0s   counter   counter
",
        rule_alpha = "─".repeat(90),
        rule_beta = "─".repeat(91),
        rule_none = "─".repeat(101),
        site = sri("site"),
        beta = sri("beta-app"),
        worker = sri("beta-app/worker"),
        counter = sri("counter"),
    );

    assert_eq!(
        show(cells, instances, &[], false, SystemTime::UNIX_EPOCH),
        expected
    );
}

#[test]
fn styles_the_app_section_rules() {
    let (cells, instances) = apped_fixture();

    let out = show(cells, instances, &[], true, SystemTime::UNIX_EPOCH);

    assert!(out.contains(&format!("{DIMMED}────{RESET} {BOLD}beta{RESET}")));
}

#[test]
fn shows_placeholders_as_na() {
    let cells = vec![PlacementEntry {
        sri: sri("pending"),
        kind: PlacementKind::Placeholder,
        app: None,
        gen_id: Gen::from_parts(1, 1),
    }];

    let expected = format!(
        "  cell  sri                                   kind  runtime  age  class  srn
{rule}
  —     {pending}  N/A   —        0s   —      —
",
        rule = "─".repeat(76),
        pending = sri("pending"),
    );

    assert_eq!(
        show(cells, vec![], &[], false, SystemTime::UNIX_EPOCH),
        expected
    );
}

/// A fading row is coloured end to end: the id column's own reset must not
/// return the rest of the row to normal.
#[test]
fn paints_highlighted_rows_end_to_end() {
    let rt = exec(runtime_id("bbcc112233445566"), "edge");
    let cells = vec![wasm(sri("sensor"), &rt), wasm(sri("pump"), &rt)];
    let instances = vec![
        instance(sri("sensor"), "sensor", None, None),
        instance(sri("pump"), "pump", None, None),
    ];
    let fading = sri("sensor");

    let out = render(cells, instances, &[], true, SystemTime::UNIX_EPOCH, &|s| {
        (*s == fading).then_some("<g>")
    });

    let row = |name: &str| out.lines().find(|l| l.contains(name)).unwrap().to_owned();
    let sensor = row("sensor");
    assert!(sensor.starts_with("<g>"), "{sensor:?}");
    assert!(sensor.ends_with(RESET), "{sensor:?}");
    assert!(sensor.contains(&format!("{RESET}<g>")), "{sensor:?}");
    assert!(!row("pump").contains("<g>"));
}

/// The row for `name` in a styled listing, with any colour it was painted.
fn painted_row(out: &str, name: &str) -> String {
    out.lines()
        .find(|l| l.contains(name))
        .unwrap_or_else(|| panic!("no row for {name} in {out:?}"))
        .to_owned()
}

/// A cell that turns up fades in, a cell that goes fades out struck through
/// and is dropped once the fade is over — or straight away when settling for
/// the final frame.
#[test]
fn arrivals_and_departures_fade_through_the_listing() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let rt = exec(runtime_id("bbcc112233445566"), "edge");
    let sensor = || {
        (
            vec![wasm(sri("sensor"), &rt)],
            vec![instance(sri("sensor"), "sensor", None, None)],
        )
    };
    let both = || {
        (
            vec![wasm(sri("sensor"), &rt), wasm(sri("pump"), &rt)],
            vec![
                instance(sri("sensor"), "sensor", None, None),
                instance(sri("pump"), "pump", None, None),
            ],
        )
    };
    let arriving = Phase::Arriving(0).sgr().unwrap();
    let departing = Phase::Departing(0).sgr().unwrap();

    let mut listing = Listing::new(vec![]);
    listing.apply(sensor(), at(0));
    assert!(painted_row(&listing.draw(at(0), true), "sensor").starts_with("  "));

    listing.apply(both(), at(100));
    let out = listing.draw(at(100), true);
    assert!(painted_row(&out, "pump").starts_with(arriving));
    assert!(painted_row(&out, "sensor").starts_with("  "));
    assert!(!listing.draw(at(100), false).contains(arriving));

    listing.apply(sensor(), at(200));
    let out = listing.draw(at(200), true);
    assert!(painted_row(&out, "pump").starts_with(departing));
    assert!(!listing.draw(at(200) + live::FADE, true).contains("pump"));

    listing.apply(sensor(), at(300));
    listing.settle();
    let out = listing.draw(at(300), true);
    assert!(!out.contains("pump"));
    assert!(painted_row(&out, "sensor").starts_with("  "));
}

/// A respawn keeps the sri and changes the generation: the row stays put and
/// fades in again rather than being replaced.
#[test]
fn a_respawn_fades_in_where_it_stands() {
    let t0 = Instant::now();
    let rt = exec(runtime_id("bbcc112233445566"), "edge");
    let incarnation = |gen_id: Gen| {
        let mut entry = wasm(sri("sensor"), &rt);
        entry.gen_id = gen_id;
        (
            vec![entry],
            vec![instance(sri("sensor"), "sensor", None, None)],
        )
    };

    let mut listing = Listing::new(vec![]);
    listing.apply(incarnation(Gen::from_parts(1, 1)), t0);
    listing.apply(
        incarnation(Gen::from_parts(2, 1)),
        t0 + Duration::from_millis(100),
    );

    let out = listing.draw(t0 + Duration::from_millis(100), true);
    assert_eq!(out.matches("sensor").count(), 3, "{out:?}"); // cell, class, srn of one row
    assert!(painted_row(&out, "sensor").starts_with(Phase::Arriving(0).sgr().unwrap()));
}

/// `m cells` is `m cells status`, so the status arguments are accepted
/// directly. They belong to status alone: another subcommand rejects them,
/// and once one is given, a later subcommand name is just another target.
#[test]
fn bare_cells_takes_the_status_arguments() {
    use clap::Parser as _;

    use crate::args::{Args, Command};
    use crate::cmd::cells::{Cells, Cmd};

    let args =
        Args::try_parse_from(["myrmic", "cells", "--once", "--interval", "1s", "sensor"]).unwrap();
    let Command::Cells(Cells { cmd: None, status }) = args.command else {
        panic!("expected bare cells");
    };
    assert!(status.live.once);
    assert_eq!(Duration::from(status.live.interval), Duration::from_secs(1));
    assert_eq!(status.targets, ["sensor"]);

    let args = Args::try_parse_from(["myrmic", "cell", "status", "--once"]).unwrap();
    let Command::Cells(Cells {
        cmd: Some(Cmd::Status(status)),
        ..
    }) = args.command
    else {
        panic!("expected cells status");
    };
    assert!(status.live.once);

    assert!(Args::try_parse_from(["myrmic", "cells", "teardown", "--once"]).is_err());

    let args = Args::try_parse_from(["myrmic", "cells", "--once", "teardown"]).unwrap();
    let Command::Cells(Cells { cmd: None, status }) = args.command else {
        panic!("expected bare cells");
    };
    assert_eq!(status.targets, ["teardown"]);
}

#[test]
fn live_is_the_default_at_two_and_a_half_seconds() {
    use clap::Parser as _;

    use crate::args::{Args, Command};
    use crate::cmd::cells::Cells;

    let args = Args::try_parse_from(["myrmic", "cells"]).unwrap();
    let Command::Cells(Cells { cmd: None, status }) = args.command else {
        panic!("expected bare cells");
    };
    assert!(!status.live.once);
    assert_eq!(
        Duration::from(status.live.interval),
        Duration::from_millis(2_500)
    );
}
