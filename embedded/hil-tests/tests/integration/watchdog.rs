//! Hardware-watchdog HIL tests (SDS-FEAT-2026-HWD-001).
//!
//! These exercise the on-die watchdog end-to-end on real silicon: a
//! `cell-wdt-selftest-logic` cell (deployed to firmware built with the
//! `wdt-selftest` feature) deliberately stalls a liveness task, the staged
//! MWDT resets the device, and the node reports the recovery into the swarm
//! watchdog-resets table after it reboots and re-registers. The test observes
//! that report over Zenoh — there is no serial-log assertion in the HIL
//! framework, and the DB report is the clean, queryable recovery signal.
//!
//! Requires firmware built with `--features wdt-selftest` (see
//! `.github/workflows/hardware-tests.yml`). Without it the `selftest` host
//! import is absent and the wedge cell fails to instantiate.

use std::time::Duration;

use cell_protocol::{
    WATCHDOG_RESETS_TABLE, WatchdogResetReason, WatchdogResetReport, scope_of_watchdog_resets,
};
use claims::assert_ok;
use test_framework::clients::db::DbHandle;
use test_framework::scenario::SwarmTestCtx;
use test_framework::swarm::SwarmProcess;

use crate::integration::{
    aot::{aot_class_name, build_aot_cell},
    device_present,
    espflash::{flash_device, flash_production_device, production_firmware_elf_path},
    hil_swarm_test,
};

const SELFTEST_CELL: &str = "cell-wdt-selftest-logic";
const SELFTEST_SRI: &str = "emb_wdt_selftest";
/// Event the self-test cell's `ping` command answers on.
const ALIVE_EVENT: &str = "wdt_selftest_alive";

/// Unprivileged cell used by the production-profile scenario. It imports
/// nothing beyond the ordinary cell surface, so what it can do to the watchdog
/// is what any shipped cell can do.
const SPIN_CELL: &str = "cell-wdt-spin-logic";
const SPIN_SRI: &str = "emb_wdt_spin";
/// Events the spin cell answers on: liveness, and completion of the busy loop.
const SPIN_ALIVE_EVENT: &str = "wdt_spin_alive";
const SPIN_DONE_EVENT: &str = "wdt_spin_done";
/// Fresh SRI for the post-reset re-deploy in the accumulation test (see
/// [`redeploy_selftest`]).
const SELFTEST_SRI_2: &str = "emb_wdt_selftest_2";

/// Budget for a full recovery: ~1 s wedge latency + the ~45 s staged-MWDT
/// escalation (30 s stage-0 + 15 s stage-1) + reboot + WiFi rejoin +
/// re-registration + the report write.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(210);

/// As above but for the required-task *stall* path, which additionally waits
/// out `stats`' 90 s staleness allowance before the MWDT window even starts.
const STALL_RECOVERY_TIMEOUT: Duration = Duration::from_secs(300);

/// A no-fault soak comfortably past the whole watchdog window (wedge latency +
/// escalation), used to prove the node does *not* spuriously reset.
const NO_RESET_SOAK: Duration = Duration::from_secs(90);

/// How long to leave the swarm up after sending the wedge command, so the
/// device picks the command out of its mailbox before the database goes away.
/// Well inside the ~45 s escalation window, so the reset still lands on a
/// swarm-less network.
const WEDGE_PROPAGATION: Duration = Duration::from_secs(10);

/// How long the network stays without a database: the rest of the escalation,
/// the reboot and WiFi rejoin, and the node's first — necessarily failing —
/// report transaction.
const REPORT_OUTAGE: Duration = Duration::from_secs(150);

/// Budget for the retained report to arrive once a database is back. The retry
/// rides on the exec re-registration round (5 min), so a full round plus swarm
/// startup and polling slack has to fit.
const RETAINED_DELIVERY_TIMEOUT: Duration = Duration::from_secs(420);

/// Every watchdog-reset report currently in the swarm DB (the test's swarm
/// starts with an empty table, so these are all from this run). Rows are
/// upserted under the reporting device's id, which the row key must therefore
/// match — a single device never accumulates rows, however often it resets.
async fn read_reports(swarm: &SwarmProcess) -> Vec<WatchdogResetReport> {
    DbHandle::new(swarm.session())
        .tb_list(scope_of_watchdog_resets(), WATCHDOG_RESETS_TABLE)
        .await
        .into_iter()
        .map(|(eid, value)| {
            let report: WatchdogResetReport =
                postcard::from_bytes(&value).expect("decode WatchdogResetReport from resets table");
            assert_eq!(
                eid,
                report.device_id.as_bytes(),
                "reset rows must be keyed on the reporting device's id",
            );
            report
        })
        .collect()
}

/// Poll the resets table until `pred` holds over the current report set, then
/// return that set; panic at the deadline.
async fn wait_for_reports_where(
    swarm: &SwarmProcess,
    timeout: Duration,
    what: &str,
    pred: impl Fn(&[WatchdogResetReport]) -> bool,
) -> Vec<WatchdogResetReport> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let reports = read_reports(swarm).await;
        if pred(&reports) {
            return reports;
        }
        assert!(
            tokio::time::Instant::now() <= deadline,
            "timed out after {timeout:?} waiting for {what}; saw {reports:?}",
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Re-deploy the self-test cell after a reset, under a *fresh* SRI. The device
/// rebooted and dropped the cell, but the swarm registry still holds the
/// original SRI bound to its now-dead runtime id, so re-using it hits
/// `DuplicateSri`; a new SRI deploys cleanly onto the re-registered runtime.
/// Retried because the device may still be rebooting.
async fn redeploy_selftest(ctx: &mut SwarmTestCtx) {
    ctx.queue_load(aot_class_name(SELFTEST_CELL), SELFTEST_SRI_2);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
    loop {
        match ctx.try_load_cells().await {
            Ok(()) => return,
            Err(e) => {
                assert!(
                    tokio::time::Instant::now() <= deadline,
                    "could not re-deploy self-test cell after reset: {e:?}"
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// Scenario 1 — the staged MWDT recovers a wedged executor and the reset is
/// reported. A guest spin-wedges the prio-1 executor; the supervisor stops
/// feeding, the MWDT resets, and after reboot the node reports `MwdtStaged`
/// with a higher reset count than before the wedge.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn staged_mwdt_reset_is_reported() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(SELFTEST_CELL)), SELFTEST_SRI)
        .spawn()
        .await;
    let _monitor = assert_ok!(flash_device());
    let ctx = spawned.connect().await;

    let baseline = staged_max_count(&read_reports(ctx.process()).await);

    // Wedge the executor; the watchdog must reset and, after reboot, the node
    // must report a *further* staged-MWDT reset.
    ctx.command_send(SELFTEST_SRI, "wedge_spin", None).await;

    let reports = wait_for_reports_where(
        ctx.process(),
        RECOVERY_TIMEOUT,
        "a staged-MWDT reset report past the baseline count",
        |rs| staged_max_count(rs) > baseline,
    )
    .await;
    let report = newest_staged(&reports).expect("a staged-MWDT report");
    assert!(
        report.reset_count >= 1,
        "reset_count should be at least 1, got {}",
        report.reset_count,
    );
}

/// Scenario 2 — no spurious reset under normal operation. Deploy the cell but
/// never wedge; soak past the entire watchdog window. The node must neither
/// report a reset nor lose the (still-loaded) cell. This is the property the
/// db-client demotion (#1014) preserves: a healthy node is never reset.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn no_spurious_reset_under_normal_operation() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(SELFTEST_CELL)), SELFTEST_SRI)
        .spawn()
        .await;
    let _monitor = assert_ok!(flash_device());
    let mut ctx = spawned.connect().await;

    // Baseline: the post-flash boot may itself have produced a report. A reset
    // during the soak would upsert that row rather than add one, so the whole
    // report has to be compared, not just how many there are.
    let baseline = read_reports(ctx.process()).await;

    tokio::time::sleep(NO_RESET_SOAK).await;

    // No reset reported during the soak ...
    let after = read_reports(ctx.process()).await;
    assert_eq!(
        after, baseline,
        "healthy node must not report a reset during the soak, saw {after:?}",
    );

    // ... and the cell is still alive (any reset would have dropped it, since a
    // rebooted node boots clean and does not auto-reload cells).
    // Commands are fire-and-forget, so the cell answers by publishing on
    // `ALIVE_EVENT`; no event arriving means the cell is gone.
    let response = ctx
        .command_await_event(SELFTEST_SRI, "ping", None, ALIVE_EVENT)
        .await;
    let alive: i32 = postcard::from_bytes(&response).expect("decode ping payload");
    assert_eq!(alive, 1, "ping should report 1");
}

/// Scenario 3 — a stalled *required* task is caught and named. A guest parks
/// the `stats` task (the sole required liveness task) forever while the
/// executor stays alive; the supervisor detects the stall after the allowance,
/// withholds the feed, and the MWDT resets. The report must name the stalled
/// task.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn stalled_required_task_is_reported_with_stale_tasks() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(SELFTEST_CELL)), SELFTEST_SRI)
        .spawn()
        .await;
    let _monitor = assert_ok!(flash_device());
    let ctx = spawned.connect().await;

    let baseline = staged_max_count(&read_reports(ctx.process()).await);

    ctx.command_send(SELFTEST_SRI, "wedge_stall", None).await;

    let reports = wait_for_reports_where(
        ctx.process(),
        STALL_RECOVERY_TIMEOUT,
        "a further staged-MWDT reset naming the stalled `stats` task",
        |rs| {
            staged_max_count(rs) > baseline
                && newest_staged(rs).is_some_and(|r| r.stale_tasks.iter().any(|t| t == "stats"))
        },
    )
    .await;
    let report = newest_staged(&reports).expect("a staged-MWDT report");
    assert!(
        report.stale_tasks.iter().any(|t| t == "stats"),
        "the newest staged report should name the stalled `stats` task, got {:?}",
        report.stale_tasks,
    );
}

/// Scenario 4 — the RTC-retained reset counter accumulates across reboots
/// (D1). Trigger two watchdog resets and assert the reported `reset_count`
/// increments by exactly one (the absolute value depends on the device's
/// history since its last power-on, so compare the delta, not the value).
///
/// The counter, not the number of rows, is what carries that history: both
/// resets are reported by the same device and therefore land on one upserted
/// row, which the test asserts as well.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn reset_count_accumulates_across_reboots() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(SELFTEST_CELL)), SELFTEST_SRI)
        .spawn()
        .await;
    let _monitor = assert_ok!(flash_device());
    let mut ctx = spawned.connect().await;

    let baseline = staged_max_count(&read_reports(ctx.process()).await);

    // First watchdog reset.
    ctx.command_send(SELFTEST_SRI, "wedge_spin", None).await;
    let first = wait_for_reports_where(
        ctx.process(),
        RECOVERY_TIMEOUT,
        "the first staged report",
        |rs| staged_max_count(rs) > baseline,
    )
    .await;
    let first_count = staged_max_count(&first);

    // Re-deploy onto the rebooted node (fresh SRI) and trigger a second reset.
    redeploy_selftest(&mut ctx).await;
    ctx.command_send(SELFTEST_SRI_2, "wedge_spin", None).await;
    let second = wait_for_reports_where(
        ctx.process(),
        RECOVERY_TIMEOUT,
        "the second staged report",
        |rs| staged_max_count(rs) > first_count,
    )
    .await;
    let second_count = staged_max_count(&second);

    assert_eq!(
        second_count,
        first_count + 1,
        "RTC-retained reset counter should increment by one across reboots: {first_count} -> {second_count}",
    );
    assert_eq!(
        second.len(),
        1,
        "both resets came from one device and must share one row, saw {second:?}",
    );
}

/// Scenario 5 — a report the node could not deliver is retained and delivered
/// once the data layer is reachable again, without a further reset
/// (REQ-FEAT-2026-HWD-SEC-001).
///
/// The failure is injected by taking the whole swarm away while the watchdog
/// escalation is already running: the node reboots into a network with no
/// database, so its first report transaction fails with "no connected
/// databases". A second, freshly spawned swarm — with an empty resets table,
/// and no re-flash, so the node keeps whatever it is still holding — must then
/// receive that report on a later registration round.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn undelivered_report_is_retained_until_the_swarm_is_back() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(SELFTEST_CELL)), SELFTEST_SRI)
        .spawn()
        .await;
    let _monitor = assert_ok!(flash_device());
    let ctx = spawned.connect().await;

    ctx.command_send(SELFTEST_SRI, "wedge_spin", None).await;
    tokio::time::sleep(WEDGE_PROPAGATION).await;

    // Dropping the ctx kills the swarm process, and with it the only database
    // on the network. The node is already wedged, so it resets with nothing
    // left to report to.
    drop(ctx);
    tokio::time::sleep(REPORT_OUTAGE).await;

    // A new swarm, and a resets table that starts empty: any report appearing
    // in it was retained by the node across the outage.
    let spawned = hil_swarm_test().spawn().await;
    let reports = wait_for_reports_where(
        spawned.process(),
        RETAINED_DELIVERY_TIMEOUT,
        "the retained watchdog report to be delivered after the outage",
        |rs| newest_staged(rs).is_some(),
    )
    .await;

    let report = newest_staged(&reports).expect("a staged-MWDT report");
    assert!(
        report.reset_count >= 1,
        "the retained report should count at least the reset it describes, got {}",
        report.reset_count,
    );
}

/// Verification fixture for `REQ-FEAT-2026-HWD-SEC-002`, production cell
/// execution: an ordinary cell running a computation that never yields must not
/// be able to stop the liveness supervisor feeding the watchdog.
///
/// Runs on a firmware built *without* `wdt-selftest`, unlike every other test in
/// this file. The requirement is about what a shipped build does, so a test of
/// it must not run on an image carrying the fault injector.
///
/// The property under test is structural: WAMR runs a cell at priority 0 and the
/// executor carrying zenoh, the wasm request handler and stats at priority 1
/// (`modem-esp32/src/main.rs`), so a preemptive scheduler keeps feeding the
/// watchdog however long a cell spins. The self-test cell has to route its wedge
/// through a host import for exactly this reason. This turns that design
/// argument into an observation.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn production_cell_cannot_starve_the_liveness_supervisor() {
    if !device_present() {
        return;
    }
    if production_firmware_elf_path().is_none() {
        eprintln!(
            "EMBEDDED_ELF_PRODUCTION not set - skipping the production-profile watchdog test"
        );
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(SPIN_CELL)), SPIN_SRI)
        .spawn()
        .await;
    let _monitor = assert_ok!(flash_production_device());
    let mut ctx = spawned.connect().await;

    // As in the soak scenario: the post-flash boot may itself have reported, and
    // a reset upserts that row rather than adding one, so compare the reports
    // themselves rather than how many there are.
    let baseline = read_reports(ctx.process()).await;

    // Occupy the WAMR thread past the entire watchdog window. The cell answers
    // on `SPIN_DONE_EVENT` once the loop has run its full duration, so arriving
    // here at all means the device stayed up throughout.
    // A command payload declared as a plain `u32` goes through the SDK's default
    // codec, which is JSON. Only the *event* payloads below are postcard, because
    // the cell encodes those itself.
    let spin_seconds = u32::try_from(NO_RESET_SOAK.as_secs()).expect("soak fits in u32");
    let payload = serde_json::to_vec(&spin_seconds).expect("encode spin duration");
    let done = ctx
        .command_await_event(SPIN_SRI, "spin", Some(payload), SPIN_DONE_EVENT)
        .await;
    let _accumulator: u64 = postcard::from_bytes(&done).expect("decode spin result");

    // No reset was reported while the cell held the runtime thread ...
    let after = read_reports(ctx.process()).await;
    assert_eq!(
        after, baseline,
        "a non-yielding cell must not cause a watchdog reset, saw {after:?}",
    );

    // ... and it is the same cell instance, not a fresh one after a reboot: a
    // reset boots the node clean and does not reload cells.
    let response = ctx
        .command_await_event(SPIN_SRI, "ping", None, SPIN_ALIVE_EVENT)
        .await;
    let alive: i32 = postcard::from_bytes(&response).expect("decode ping payload");
    assert_eq!(alive, 1, "the cell should have survived the spin");
}

/// The staged-MWDT report with the highest `reset_count`. One device upserts a
/// single row, so with one device under test this is that row — whenever it
/// last reported a staged reset.
fn newest_staged(reports: &[WatchdogResetReport]) -> Option<&WatchdogResetReport> {
    reports
        .iter()
        .filter(|r| r.last_reason == WatchdogResetReason::MwdtStaged)
        .max_by_key(|r| r.reset_count)
}

/// Highest `reset_count` among the staged-MWDT reports, `0` when there are
/// none. Flashing the device hard-resets it, which the node may itself
/// classify and report, so tests wait for this count to rise above a baseline
/// rather than assuming an empty table; the *staged* path in particular is
/// only produced by our deliberate wedges.
fn staged_max_count(reports: &[WatchdogResetReport]) -> u32 {
    newest_staged(reports).map_or(0, |r| r.reset_count)
}
