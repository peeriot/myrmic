use embassy_time::{Duration as EmbassyDuration, TICK_HZ};

/// Convert a `core::time::Duration` into an `embassy_time::Duration`,
/// saturating at `embassy_time::Duration`'s maximum representable value.
pub(crate) fn to_embassy(d: core::time::Duration) -> EmbassyDuration {
    // ticks from whole seconds (saturating on overflow).
    let secs_ticks = d.as_secs().saturating_mul(TICK_HZ);

    // ticks from the sub-second part, all in u64.
    // subsec_nanos < 1e9, so `subsec_nanos * TICK_HZ` cannot overflow u64
    // for any TICK_HZ up to ~1.8e10 (18 GHz) — far above any embassy tick rate.
    let subsec_ticks = u64::from(d.subsec_nanos()) * TICK_HZ / 1_000_000_000;

    EmbassyDuration::from_ticks(secs_ticks.saturating_add(subsec_ticks))
}
