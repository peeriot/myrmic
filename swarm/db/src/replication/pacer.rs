//! Paces a deep pull by what its own pages cost.
//!
//! The puller drives the transfer, so resting between pages is the only brake
//! on how hard it leans on the holder's link and its own storage. A fixed rest
//! is either flat out for a small node or needlessly slow for a fast one.
//! Instead the rest is a share of each page's cost, and the share adapts: it
//! shrinks while pages stay as cheap as they have been, and grows back the
//! moment they get dearer — the first sign the link or a disk is saturating.

use std::time::Duration;

/// The rest-to-work ratio a pull starts at: a 50% duty cycle, until the pages
/// show what the pair can take.
const START_RATIO: f64 = 1.0;
/// The least a pull ever rests, as a share of a page's cost (~95% duty).
const MIN_RATIO: f64 = 0.05;
/// Applied per page whose cost holds steady.
const RAMP: f64 = 0.75;
/// Applied per page whose cost has climbed.
const BACKOFF: f64 = 2.0;
/// How far recent cost may rise over the baseline before it counts as climbing.
const CLIMB: f64 = 1.5;
/// Smoothing for the baseline: slow, so saturation can't drag it up unnoticed.
const BASELINE_WEIGHT: f64 = 0.1;
/// Smoothing for recent cost: quick, but not so quick one slow page trips it.
const RECENT_WEIGHT: f64 = 0.5;
/// Pages are compared per byte, but a tiny page is mostly fixed overhead;
/// flooring its size keeps it from looking expensive.
const MIN_PAGE_BYTES: usize = 4 * 1024;

#[derive(Debug)]
pub(crate) struct PullPacer {
    ratio: f64,
    /// Seconds per byte, as (baseline, recent) moving averages.
    cost: Option<(f64, f64)>,
}

impl PullPacer {
    pub(crate) fn new() -> Self {
        Self {
            ratio: START_RATIO,
            cost: None,
        }
    }

    /// How long to rest after a page of `bytes` that took `took` to fetch and
    /// apply.
    pub(crate) fn rest_after(&mut self, took: Duration, bytes: usize) -> Duration {
        #[expect(clippy::cast_precision_loss, reason = "a page is far below 2^52 bytes")]
        let per_byte = took.as_secs_f64() / bytes.max(MIN_PAGE_BYTES) as f64;

        match &mut self.cost {
            None => self.cost = Some((per_byte, per_byte)),
            Some((baseline, recent)) => {
                *recent += RECENT_WEIGHT * (per_byte - *recent);
                if *recent > *baseline * CLIMB {
                    self.ratio = (self.ratio * BACKOFF).min(START_RATIO);
                } else {
                    self.ratio = (self.ratio * RAMP).max(MIN_RATIO);
                }
                *baseline += BASELINE_WEIGHT * (per_byte - *baseline);
            }
        }

        took.mul_f64(self.ratio)
    }

    #[cfg(test)]
    fn ratio(&self) -> f64 {
        self.ratio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: usize = 64 * 1024;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_first_page_rests_as_long_as_it_took() {
        let mut pacer = PullPacer::new();
        assert_eq!(pacer.rest_after(ms(20), PAGE), ms(20));
    }

    #[test]
    fn steady_pages_ramp_down_to_the_floor() {
        let mut pacer = PullPacer::new();
        for _ in 0..50 {
            pacer.rest_after(ms(20), PAGE);
        }
        assert!((pacer.ratio() - MIN_RATIO).abs() < f64::EPSILON);
        assert_eq!(pacer.rest_after(ms(20), PAGE), ms(1));
    }

    #[test]
    fn pages_getting_dearer_back_off() {
        let mut pacer = PullPacer::new();
        for _ in 0..50 {
            pacer.rest_after(ms(20), PAGE);
        }

        // Saturation: the same pages now take four times as long.
        let mut rests = Vec::new();
        for _ in 0..4 {
            rests.push(pacer.rest_after(ms(80), PAGE));
        }
        assert!(
            rests.windows(2).all(|pair| pair[1] > pair[0]),
            "each dearer page must rest longer: {rests:?}"
        );
        assert!(pacer.ratio() > 0.5, "backed well off the floor");
    }

    #[test]
    fn one_slow_page_is_not_saturation() {
        let mut pacer = PullPacer::new();
        for _ in 0..50 {
            pacer.rest_after(ms(20), PAGE);
        }
        pacer.rest_after(ms(35), PAGE);
        assert!((pacer.ratio() - MIN_RATIO).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_is_compared_per_byte() {
        let mut pacer = PullPacer::new();
        for _ in 0..50 {
            pacer.rest_after(ms(20), PAGE);
        }
        // Twice the page in twice the time is the same rate, not a climb.
        pacer.rest_after(ms(40), 2 * PAGE);
        assert!((pacer.ratio() - MIN_RATIO).abs() < f64::EPSILON);
    }

    #[test]
    fn the_ratio_never_exceeds_the_start() {
        let mut pacer = PullPacer::new();
        pacer.rest_after(ms(1), PAGE);
        for step in 1..20 {
            pacer.rest_after(ms(step * 100), PAGE);
        }
        assert!(pacer.ratio() <= START_RATIO);
    }
}
