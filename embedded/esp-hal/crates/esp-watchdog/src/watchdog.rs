//! On-die watchdog wiring — the enforcement layer of the hardware watchdog
//! (SDS-FEAT-2026-HWD-001, Areas B1 + C1).
//!
//! Two independent layers, both fed by the watchdog feeder
//! ([`crate::liveness`]):
//!
//! - **MWDT on TIMG1** — the task-liveness watchdog, staged (C1): stage 0
//!   fires an interrupt (minimal handler; the RTC hang record arrives with
//!   #1013), stage 1 performs `ResetSystem` — the only action that yields the
//!   clean power-on-equivalent state the feature requires. TIMG0 is owned by
//!   the esp-rtos tick, TIMG1 is free (SDS constraint 7).
//! - **RWDT** (RTC power domain) — the independent last-resort backstop (B1)
//!   with a longer single-stage `ResetSystem` timeout; it catches the case
//!   where the MWDT path itself is compromised (clock/timer-group fault).
//!
//! MWDT stage timeouts are sequential: stage 1 starts counting when stage 0
//! expires, so the reset lands `STAGE0 + STAGE1` after the last feed.
//!
//! All timeouts are platform-defined (no operator configuration surface) and
//! provisional until #1014 characterizes worst-case stalls under radio load.
//! They compose with the liveness allowances: a stalled required task first
//! exhausts its allowance (feed withheld), then the MWDT window elapses.

use core::cell::RefCell;

use cell_protocol::WatchdogResetReason;
use critical_section::Mutex;
use esp_hal::interrupt::InterruptConfigurable;
use esp_hal::peripherals::TIMG1;
use esp_hal::rtc_cntl::{Rwdt, RwdtStage, RwdtStageAction, SocResetReason};
use esp_hal::system::Cpu;
use esp_hal::time::Duration;
use esp_hal::timer::timg::{MwdtStage, MwdtStageAction, Wdt};

use self::record_storage::{read_record, write_record};

/// MWDT stage 0 → interrupt (hang record): 30 s after the last feed, i.e. six
/// missed 5 s feeder rounds.
const MWDT_STAGE0_TIMEOUT: Duration = Duration::from_secs(30);
/// MWDT stage 1 → `ResetSystem`: a further 15 s after stage 0 fired, leaving
/// the stage-0 handler ample time to persist the hang record (#1013).
const MWDT_STAGE1_TIMEOUT: Duration = Duration::from_secs(15);
/// RWDT backstop → `ResetSystem`: outlives the whole MWDT escalation
/// (30 s + 15 s) so it only ever acts when the MWDT path failed.
const RWDT_TIMEOUT: Duration = Duration::from_secs(60);
/// Unlock key for the TIMG WDT configuration registers; `0` re-arms the
/// write protection.
const MWDT_WKEY: u32 = 0x50D8_3AA1;

/// Armed watchdog handles, populated once by [`arm`]. The critical-section
/// mutex covers the two access contexts: the feeder task ([`feed`]) and —
/// for the MWDT — nothing else at runtime (the stage-0 ISR touches only the
/// interrupt-clear register, not the driver).
static RWDT: Mutex<RefCell<Option<Rwdt>>> = Mutex::new(RefCell::new(None));
static MWDT: Mutex<RefCell<Option<Wdt<TIMG1<'static>>>>> = Mutex::new(RefCell::new(None));

/// Identifies a valid [`HangRecord`]; bumped on layout changes so a record
/// written by older firmware is discarded rather than misread.
const RECORD_MAGIC: u32 = 0x5744_4731; // "WDG1"
/// [`HangRecord::evidence`] value set by the stage-0 interrupt; consumed
/// (cleared) by [`report_boot`] exactly once.
const EVIDENCE_FRESH: u32 = 1;

/// Hang record kept in the always-on power domain (SDS Area D / #1013):
/// survives `ResetSystem`, zeroed only when that domain loses power. The
/// stage-0 interrupt writes the hang evidence; [`report_boot`] owns the reset
/// counter and consumes the evidence after the reboot. Guarded by magic +
/// checksum against first-boot garbage and torn writes.
///
/// This is the in-memory shape; where it is retained depends on the chip -
/// see [`read_record`].
#[repr(C)]
#[derive(Clone, Copy)]
struct HangRecord {
    magic: u32,
    /// Watchdog resets since the node last lost power (counted at boot from
    /// the hardware reset reason, so backstop resets are counted too).
    reset_count: u32,
    /// [`EVIDENCE_FRESH`] when the fields below were written by the stage-0
    /// interrupt and not yet consumed by a boot report.
    evidence: u32,
    /// Node uptime when the hang was recorded.
    uptime_ms: u64,
    /// Stale-required-task bitmask from the feeder's last round
    /// (`0` = the executor itself was wedged).
    stale_mask: u32,
    checksum: u32,
}

impl HangRecord {
    /// The zeroed record. Its `magic` fails validation, so [`report_boot`]
    /// treats it as the first boot since the always-on domain lost power.
    const EMPTY: Self = Self {
        magic: 0,
        reset_count: 0,
        evidence: 0,
        uptime_ms: 0,
        stale_mask: 0,
        checksum: 0,
    };
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "deliberate 32-bit folding of the u64 into the checksum"
)]
fn record_checksum(r: &HangRecord) -> u32 {
    r.magic
        .wrapping_add(r.reset_count)
        .wrapping_add(r.evidence)
        .wrapping_add(r.uptime_ms as u32)
        .wrapping_add((r.uptime_ms >> 32) as u32)
        .wrapping_add(r.stale_mask)
}

/// The boot-time watchdog findings, pending until the db-client has reported
/// them to the swarm (snapshot via [`peek_boot_report`], dropped via
/// [`clear_boot_report`]).
#[derive(Clone, Copy)]
pub struct BootWatchdogReport {
    /// Watchdog resets since the node last lost power.
    pub reset_count: u32,
    /// Which watchdog layer performed the reset.
    pub reason: WatchdogResetReason,
    /// Uptime at the recorded hang, when stage-0 evidence exists.
    pub uptime_ms: Option<u64>,
    /// Stale-required-task bitmask from the hang evidence (`0` otherwise).
    pub stale_mask: u32,
}

/// The report this boot owes the swarm, held until the swarm has accepted it.
///
/// Deliberately plain RAM rather than retained memory: a watchdog reset wipes
/// it, and the boot that follows re-queues it from the [`HangRecord`] with the
/// incremented reset count. An undelivered report therefore cannot outlive the
/// condition it describes — the slot only ever holds the newest state.
static PENDING_REPORT: Mutex<RefCell<Option<BootWatchdogReport>>> = Mutex::new(RefCell::new(None));

/// Snapshot the pending boot report, if this boot followed a watchdog reset.
///
/// Non-consuming: the report stays pending so a delivery that fails can be
/// retried. Only [`clear_boot_report`] drops it.
pub fn peek_boot_report() -> Option<BootWatchdogReport> {
    critical_section::with(|cs| *PENDING_REPORT.borrow_ref(cs))
}

/// Drop the pending boot report now that the swarm has accepted `delivered`.
///
/// A pending report covering more resets than the delivered one is kept: only
/// state that actually reached the swarm is discarded.
pub fn clear_boot_report(delivered: &BootWatchdogReport) {
    critical_section::with(|cs| {
        let mut pending = PENDING_REPORT.borrow_ref_mut(cs);
        if pending.is_some_and(|p| p.reset_count <= delivered.reset_count) {
            *pending = None;
        }
    });
}

/// Inspect the hardware reset reason and the RTC hang record; if this boot
/// follows a watchdog reset, log it and queue the swarm report (D1: recovery
/// must be observable, reset loops must be countable). The queued report stays
/// pending until the db-client confirms the swarm accepted it. Call once at
/// startup, before [`arm`].
pub fn report_boot() {
    let wd_reason = match esp_hal::rtc_cntl::reset_reason(Cpu::ProCpu) {
        // Our staged MWDT lives on TIMG1.
        Some(SocResetReason::CoreMwdt1 | SocResetReason::Cpu0Mwdt1) => {
            Some(WatchdogResetReason::MwdtStaged)
        }
        Some(
            SocResetReason::CoreRtcWdt | SocResetReason::Cpu0RtcWdt | SocResetReason::SysRtcWdt,
        ) => Some(WatchdogResetReason::RwdtBackstop),
        _ => None,
    };

    let mut record = read_record();
    if record.magic != RECORD_MAGIC || record.checksum != record_checksum(&record) {
        // First boot since the RTC domain lost power (or a layout change) —
        // start a fresh record. This is also what resets the counter, giving
        // it "since last power-on" semantics.
        record = HangRecord {
            magic: RECORD_MAGIC,
            reset_count: 0,
            evidence: 0,
            uptime_ms: 0,
            stale_mask: 0,
            checksum: 0,
        };
    }

    if let Some(reason) = wd_reason {
        record.reset_count = record.reset_count.wrapping_add(1);

        // Evidence applies only to the staged path: the backstop fires
        // precisely when the stage-0 interrupt could not run.
        let evidence =
            record.evidence == EVIDENCE_FRESH && reason == WatchdogResetReason::MwdtStaged;
        let uptime_ms = evidence.then_some(record.uptime_ms);
        let stale_mask = if evidence { record.stale_mask } else { 0 };

        match (reason, uptime_ms) {
            (WatchdogResetReason::MwdtStaged, Some(uptime)) if stale_mask != 0 => {
                let stale: alloc::vec::Vec<&str> = crate::liveness::names_of(stale_mask).collect();
                log::warn!(
                    "[watchdog] recovered from watchdog reset #{} — hang at uptime {} ms, stale tasks {:?}",
                    record.reset_count,
                    uptime,
                    stale,
                );
            }
            (WatchdogResetReason::MwdtStaged, Some(uptime)) => {
                log::warn!(
                    "[watchdog] recovered from watchdog reset #{} — hang at uptime {} ms, executor wedged",
                    record.reset_count,
                    uptime,
                );
            }
            (WatchdogResetReason::MwdtStaged, None) => {
                log::warn!(
                    "[watchdog] recovered from watchdog reset #{} — no hang record (stage-0 evidence missing)",
                    record.reset_count,
                );
            }
            (WatchdogResetReason::RwdtBackstop, _) => {
                log::warn!(
                    "[watchdog] recovered from RWDT BACKSTOP reset #{} — the staged MWDT path failed",
                    record.reset_count,
                );
            }
        }

        critical_section::with(|cs| {
            PENDING_REPORT
                .borrow_ref_mut(cs)
                .replace(BootWatchdogReport {
                    reset_count: record.reset_count,
                    reason,
                    uptime_ms,
                    stale_mask,
                });
        });
    }

    // Evidence is consumed (or stale from an interrupted escalation, e.g. a
    // reflash between stage 0 and stage 1) — clear it either way.
    record.evidence = 0;
    write_record(record);
}

/// Configure and enable both watchdogs. Call once at startup, before the
/// watchdog feeder is spawned; from then on only a healthy feeder
/// keeps the device from resetting.
pub fn arm(mut rwdt: Rwdt, mut mwdt: Wdt<TIMG1<'static>>) {
    // Ordering is load-bearing (verified on hardware): `enable()` RESETS the
    // stage configuration to its built-in defaults (stage 0 ResetSystem,
    // stages 1-3 Off), so the stages must be configured AFTER enabling, not
    // before. None of the `wdtconfig` writes reach the watchdog state machine
    // until [`mwdt_latch_config`] pulses the update bit, which is why that
    // call closes the block.
    // Also: the two stage timeouts share one clock prescaler (`set_timeout`
    // recomputes it per call, last call wins) — keep both timeouts in the
    // same prescaler range (≤ ~107 s at 40 MHz APB) or stage 0 gets silently
    // rescaled.

    // MWDT: staged escalation (C1) — interrupt, then full system reset.
    mwdt.set_interrupt_handler(mwdt_stage0);
    mwdt.enable();
    mwdt.feed();
    mwdt.set_stage_action(MwdtStage::Stage0, MwdtStageAction::Interrupt);
    mwdt.set_stage_action(MwdtStage::Stage1, MwdtStageAction::ResetSystem);
    mwdt.set_timeout(MwdtStage::Stage0, MWDT_STAGE0_TIMEOUT);
    mwdt.set_timeout(MwdtStage::Stage1, MWDT_STAGE1_TIMEOUT);
    mwdt_latch_config();
    // esp-hal wraps no peripheral-side enable for the WDT interrupt line —
    // set it directly.
    TIMG1::regs().int_ena().modify(|_, w| w.wdt().set_bit());
    mwdt.feed();

    // RWDT: single-stage backstop (B1). `enable()`'s stomped default is
    // already stage 0 = ResetSystem; the explicit call documents intent.
    rwdt.enable();
    rwdt.set_stage_action(RwdtStage::Stage0, RwdtStageAction::ResetSystem);
    rwdt.set_timeout(RwdtStage::Stage0, RWDT_TIMEOUT);
    rwdt.feed();

    critical_section::with(|cs| {
        RWDT.borrow_ref_mut(cs).replace(rwdt);
        MWDT.borrow_ref_mut(cs).replace(mwdt);
    });

    log::info!(
        "[watchdog] armed: MWDT stage0 interrupt {} s + stage1 reset {} s; RWDT backstop {} s",
        MWDT_STAGE0_TIMEOUT.as_secs(),
        MWDT_STAGE1_TIMEOUT.as_secs(),
        RWDT_TIMEOUT.as_secs(),
    );
}

/// Feed both watchdogs. Passed to the watchdog feeder as its feed hook —
/// invoked only while every required task proves progress.
pub fn feed() {
    critical_section::with(|cs| {
        if let Some(mwdt) = MWDT.borrow_ref_mut(cs).as_mut() {
            mwdt.feed();
        }
        if let Some(rwdt) = RWDT.borrow_ref_mut(cs).as_mut() {
            rwdt.feed();
        }
    });
}

/// Latch the staged MWDT configuration into the watchdog's own clock domain.
///
/// Writes to `wdtconfig0..5` are held in shadow registers until `WDT_CONF_UPDATE_EN` is pulsed;
/// only then do the stage actions and stage timeouts reach the watchdog state machine. esp-hal
/// pulses the bit from `set_timeout`, but behind a hard-coded chip list that covers the C2, C3 and
/// C6 only.
///
/// Pulsing here covers every supported chip. The bit is a one-shot trigger, so the extra pulse is a
/// no-op where esp-hal already issued one.
fn mwdt_latch_config() {
    let regs = TIMG1::regs();

    // SAFETY: `bits` on the key field is the documented unlock/lock sequence
    // for the WDT configuration registers; the register holds nothing else.
    unsafe {
        regs.wdtwprotect().write(|w| w.wdt_wkey().bits(MWDT_WKEY));
        regs.wdtconfig0()
            .modify(|_, w| w.wdt_conf_update_en().set_bit());
        regs.wdtwprotect().write(|w| w.wdt_wkey().bits(0));
    }
}

/// MWDT stage-0 interrupt: the runtime is presumed wedged, the hardware reset
/// (stage 1) is already scheduled and fires regardless of what happens here.
/// Keep this minimal and self-contained (SDS C1): persist the hang evidence
/// into RTC-retained memory first, then everything else.
#[esp_hal::handler]
fn mwdt_stage0() {
    // The record was validated/initialized by `report_boot` at startup; only
    // the evidence fields change here, the reset counter stays boot-owned.
    let mut record = read_record();
    record.magic = RECORD_MAGIC;
    record.evidence = EVIDENCE_FRESH;
    record.uptime_ms = embassy_time::Instant::now().as_millis();
    record.stale_mask = crate::liveness::stale_mask();
    write_record(record);

    // Clear the peripheral interrupt flag so the handler doesn't retrigger
    // during the stage-1 window.
    TIMG1::regs()
        .int_clr()
        .write(|w| w.wdt().clear_bit_by_one());
    // The uptime doubles as a stage-timing check: a stage 0 that fires far earlier than
    // `MWDT_STAGE0_TIMEOUT` means the timeouts never latched.
    log::error!(
        "[watchdog] liveness lost — stage-0 fired at uptime {} ms, system reset in stage 1",
        record.uptime_ms,
    );
}

/// Retained storage for the [`HangRecord`] (SDS Area D / #1013).
///
/// The record has to outlive `ResetSystem` but not a power cycle, so it lives in the always-on
/// power domain. What that domain offers as storage is chip-specific, and this module is the only
/// place that difference is expressed:
///
/// - Chips with `rtc_fast` RAM (ESP32-C5, ESP32-C6) keep the struct as-is in a persistent static.
/// - The ESP32-C61 has no `rtc_fast` RAM, so the record is packed into two `LP_AON` scratch
///   registers instead.
///
/// Both backends have the same two access contexts, and they cannot overlap:
/// [`crate::watchdog::report_boot`] runs in `main` before [`crate::watchdog::arm`] enables the watchdog, and the
/// stage-0 interrupt can only fire after `arm`.
#[cfg(feature = "hang-record")]
mod record_storage {
    #[cfg(feature = "esp32c61")]
    use esp_hal::peripherals::LP_AON;

    #[cfg(feature = "esp32c61")]
    use super::{EVIDENCE_FRESH, RECORD_MAGIC};
    use super::{HangRecord, record_checksum};

    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61")))]
    compile_error!("`hang-record` needs a retained-storage backend for this chip");

    /// Tag in the high half of the control word, matched on read so a
    /// never-written (power-cycled) register pair is rejected.
    #[cfg(feature = "esp32c61")]
    const AON_TAG: u32 = RECORD_MAGIC >> 16;
    /// Hang-evidence flag in the payload word.
    #[cfg(feature = "esp32c61")]
    const AON_EVIDENCE: u32 = 1 << 7;
    /// Stale-task bitmask field of the payload word - one bit per `liveness::Task`, which is why
    /// the field is 7 bits wide.
    #[cfg(feature = "esp32c61")]
    const AON_STALE_MASK: u32 = 0x7F;

    /// Highest `liveness::Task` discriminant used by `liveness::REQUIRED` - the only tasks whose
    /// bit ever reaches `AON_STALE_MASK`. Must stay below the mask's width or a required task's
    /// stale bit collides with `AON_EVIDENCE` in the packed payload.
    #[cfg(feature = "esp32c61")]
    const fn max_required_discriminant() -> usize {
        let mut max = 0;
        let mut i = 0;
        while i < crate::liveness::REQUIRED.len() {
            let discriminant = crate::liveness::REQUIRED[i].0 as usize;
            if discriminant > max {
                max = discriminant;
            }
            i += 1;
        }

        max
    }

    #[cfg(feature = "esp32c61")]
    static_assertions::const_assert!(max_required_discriminant() < 7);
    /// Largest uptime the 24-bit seconds field holds (~194 days); a hang
    /// beyond it is recorded saturated.
    #[cfg(feature = "esp32c61")]
    const AON_UPTIME_MAX_S: u32 = 0x00FF_FFFF;

    // SAFETY: inhabited plain-integer struct — every bit pattern is a valid value (magic/checksum
    //         validation handles semantic garbage), and all fields are `Persistable` primitives.
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    unsafe impl esp_hal::Persistable for HangRecord {}

    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    #[esp_hal::ram(unstable(rtc_fast, persistent))]
    static mut HANG_RECORD: HangRecord = HangRecord::EMPTY;

    /// Read a snapshot of the record out of `rtc_fast` persistent RAM.
    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub(super) fn read_record() -> HangRecord {
        // SAFETY: see the module docs — the two access contexts cannot overlap, so there is no
        //         concurrent aliasing of the static.
        unsafe { (&raw const HANG_RECORD).read_volatile() }
    }

    #[cfg(any(feature = "esp32c5", feature = "esp32c6"))]
    pub(super) fn write_record(mut r: HangRecord) {
        r.checksum = record_checksum(&r);
        // SAFETY: see `read_record` — the two access contexts cannot overlap,
        //         so there is no concurrent aliasing of the static.
        unsafe { (&raw mut HANG_RECORD).write_volatile(r) }
    }

    /// Read a snapshot of the record out of the `LP_AON` scratch registers.
    ///
    /// Those registers sit in the same always-on domain as the `rtc_fast` RAM the other chips use,
    /// so they survive `ResetSystem` the same way. Two of them hold the record in packed form,
    /// which narrows the fields that do not fit: uptime to whole seconds, the reset count to 8
    /// bits.
    ///
    /// `store8`/`store9` are the two registers ESP-IDF's own scratch-register map leaves
    /// unassigned. The pair reads back as "no record" unless the tag and the checksum both match,
    /// so a power cycle, a foreign write or a torn write degrades to a missing record rather than
    /// to a bogus one.
    #[cfg(feature = "esp32c61")]
    pub(super) fn read_record() -> HangRecord {
        let payload = LP_AON::regs().store8().read().lp_aon_store8().bits();
        let control = LP_AON::regs().store9().read().lp_aon_store9().bits();
        let reset_count = (control >> 8) & 0xFF;

        if control >> 16 != AON_TAG || control & 0xFF != aon_checksum(payload, reset_count) {
            return HangRecord::EMPTY;
        }

        let mut record = HangRecord {
            magic: RECORD_MAGIC,
            reset_count,
            evidence: u32::from(payload & AON_EVIDENCE != 0),
            uptime_ms: u64::from(payload >> 8) * 1000,
            stale_mask: payload & AON_STALE_MASK,
            checksum: 0,
        };
        record.checksum = record_checksum(&record);

        record
    }

    #[cfg(feature = "esp32c61")]
    pub(super) fn write_record(r: HangRecord) {
        let uptime_s = u32::try_from(r.uptime_ms / 1000)
            .unwrap_or(AON_UPTIME_MAX_S)
            .min(AON_UPTIME_MAX_S);
        let evidence = if r.evidence == EVIDENCE_FRESH {
            AON_EVIDENCE
        } else {
            0
        };
        let payload = (uptime_s << 8) | evidence | (r.stale_mask & AON_STALE_MASK);
        let reset_count = r.reset_count.min(0xFF);
        let control = (AON_TAG << 16) | (reset_count << 8) | aon_checksum(payload, reset_count);

        // Payload first, control word last: the checksum covers the payload, so a reset landing
        // between the two writes leaves a mismatching pair that reads back as "no record" instead
        // of as stale evidence.
        // SAFETY: `bits` writes the full scratch register, which has no reserved fields and no
        //         side effects beyond storing the value.
        unsafe {
            LP_AON::regs()
                .store8()
                .write(|w| w.lp_aon_store8().bits(payload));
            LP_AON::regs()
                .store9()
                .write(|w| w.lp_aon_store9().bits(control));
        }
    }

    /// Fold the payload and reset count into the 8-bit checksum carried by
    /// the control word.
    #[cfg(feature = "esp32c61")]
    fn aon_checksum(payload: u32, reset_count: u32) -> u32 {
        let folded = payload ^ (payload >> 8) ^ (payload >> 16) ^ (payload >> 24) ^ reset_count;

        folded & 0xFF
    }
}

/// Storage stub for builds without `hang-record`: the watchdog still detects hangs and resets, but
/// nothing about them survives the reset.
#[cfg(not(feature = "hang-record"))]
mod record_storage {
    use super::HangRecord;

    /// Always reads back as "no record", so [`super::report_boot`] starts a fresh one every boot
    /// and never finds hang evidence.
    pub(super) fn read_record() -> HangRecord {
        HangRecord::EMPTY
    }

    pub(super) fn write_record(record: HangRecord) {
        let _ = record;
    }
}
