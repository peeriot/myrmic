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
        render(
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
        render(cells, instances, &[], false, SystemTime::UNIX_EPOCH),
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
        render(cells, instances, &[], false, SystemTime::UNIX_EPOCH),
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
        render(cells, instances, &targets, false, SystemTime::UNIX_EPOCH),
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
        render(cells, instances, &targets, false, SystemTime::UNIX_EPOCH),
        expected
    );
}

#[test]
fn reports_an_empty_registry() {
    assert_eq!(
        render(vec![], vec![], &[], false, SystemTime::UNIX_EPOCH),
        "No cells registered\n"
    );
}

#[test]
fn highlights_the_unique_runtime_prefix_when_styled() {
    let (cells, instances) = tree_fixture();

    let out = render(cells, instances, &[], true, SystemTime::UNIX_EPOCH);

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
        render(cells, instances, &[], false, SystemTime::UNIX_EPOCH),
        expected
    );
}

#[test]
fn styles_the_app_section_rules() {
    let (cells, instances) = apped_fixture();

    let out = render(cells, instances, &[], true, SystemTime::UNIX_EPOCH);

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
        render(cells, vec![], &[], false, SystemTime::UNIX_EPOCH),
        expected
    );
}
