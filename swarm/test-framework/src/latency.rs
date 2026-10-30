#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

use std::{
    collections::BTreeMap,
    fmt::Display,
    ops::{Deref, DerefMut},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cell_protocol::Sri;
use swarm_telemetry::db::opentelemetry_proto::tonic::trace::v1::Span;

use crate::SriAttribute;

#[derive(Debug)]
pub struct CellData {
    /// the SRI of the cell
    pub sri: Sri,
    /// the offset to t=0 (when the full latency measurement started)
    pub offset: Duration,
    /// total message handling time
    pub duration: Duration,
}

/// Latency breakdown computed from a set of spans belonging to one trace.
#[derive(Debug)]
pub struct Latency {
    /// total elapsed time from t=0 to the last span's end.
    pub full: Duration,
    /// measurements for cells that were travelled during the latency measurement. this involves
    /// offsets to t=0 (when the cell started processing) and the total handling time of the cells.
    pub cells: Vec<CellData>,
}

impl Latency {
    /// use this function when the load producer was a cell inside of the swarm and we are only
    /// looking at spans.
    #[must_use]
    pub fn new(spans: &[Span]) -> Self {
        Self::compute(spans, None)
    }

    /// use this function when the load producer was outside of the swarm and provides an external
    /// start time (t=0)
    #[must_use]
    pub fn new_with_t0(spans: &[Span], t0: SystemTime) -> Self {
        let start_nanos = t0
            .duration_since(UNIX_EPOCH)
            .expect("external_start must not be before the Unix epoch")
            .as_nanos()
            .try_into()
            .expect("external_start is too far in the future to fit in a u64 nanosecond count");
        Self::compute(spans, Some(start_nanos))
    }

    // compute timings of collected spans
    fn compute(spans: &[Span], t0: Option<u64>) -> Self {
        let tagged = spans
            .iter()
            .filter_map(|span| {
                Some((
                    span.sri()?,
                    span.start_time_unix_nano,
                    span.end_time_unix_nano,
                ))
            })
            .collect::<Vec<_>>();

        // spans can arrive out of temporal order (e.g. queried back from a DB that merges
        // writes from several replicas), so t0 must be the earliest start seen rather than
        // whichever span happened to be first in the slice — otherwise an earlier-starting
        // span later in iteration order underflows its offset against t0.
        let t0 = t0.or_else(|| tagged.iter().map(|(_, start, _)| *start).min());
        let end = tagged
            .iter()
            .map(|(_, _, end)| *end)
            .max()
            .unwrap_or(t0.unwrap_or(0));

        let cells = tagged
            .into_iter()
            .map(|(sri, start, end)| CellData {
                sri,
                offset: Duration::from_nanos(start.saturating_sub(t0.unwrap_or(0))),
                duration: Duration::from_nanos(end.saturating_sub(start)),
            })
            .collect();

        Self {
            full: Duration::from_nanos(end.saturating_sub(t0.unwrap_or(0))),
            cells,
        }
    }
}

impl FromIterator<Latency> for LatencyCollection {
    fn from_iter<T: IntoIterator<Item = Latency>>(iter: T) -> Self {
        let mut cells = BTreeMap::new();
        let mut full = DurationCollection::default();

        for latency in iter {
            full.push(latency.full.as_nanos() as f64);

            for cell in latency.cells {
                let entry = cells
                    .entry(cell.sri)
                    .or_insert(CellLatencyCollection::default());
                entry.cell_durations.push(cell.duration.as_nanos() as f64);
                entry.cell_starts.push(cell.offset.as_nanos() as f64);
                entry
                    .cell_ends
                    .push((cell.offset + cell.duration).as_nanos() as f64);
            }
        }

        Self { full, cells }
    }
}

pub struct LatencyCollection {
    full: DurationCollection,
    cells: BTreeMap<Sri, CellLatencyCollection>,
}

impl LatencyCollection {
    pub fn distribution(self) -> LatencyDistribution {
        LatencyDistribution {
            full: self.full.distribution(),
            cells: self
                .cells
                .into_iter()
                .map(|(key, value)| (key, value.distribution()))
                .collect(),
        }
    }
}

#[derive(Default)]
pub struct CellLatencyCollection {
    cell_starts: DurationCollection,
    cell_ends: DurationCollection,
    cell_durations: DurationCollection,
}

impl CellLatencyCollection {
    pub fn distribution(self) -> CellLatencyDistribution {
        CellLatencyDistribution {
            starts: self.cell_starts.distribution(),
            ends: self.cell_ends.distribution(),
            durations: self.cell_durations.distribution(),
        }
    }
}

#[derive(Default)]
pub struct DurationCollection(
    // nano second values as f64
    Vec<f64>,
);

impl DerefMut for DurationCollection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Deref for DurationCollection {
    type Target = Vec<f64>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DurationCollection {
    /// Computes the distribution, or all-zero/`samples: 0` if there are no samples — e.g. a
    /// pass whose producer runs entirely inside the swarm dispatches no call from the harness
    /// for [`Latency::new_with_t0`] to measure, so this must tolerate zero samples rather than
    /// treat it as a calculation error.
    #[must_use]
    pub fn distribution(mut self) -> DurationDistribution {
        if self.0.is_empty() {
            return DurationDistribution {
                samples: 0,
                mean: Duration::ZERO,
                median: Duration::ZERO,
                p95: Duration::ZERO,
                p99: Duration::ZERO,
                std_deviation: Duration::ZERO,
            };
        }

        let n = self.0.len() as f64;
        let mean = self.0.iter().sum::<f64>() / n;
        let variance = self.0.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
        let std_deviation = variance.sqrt();

        self.0
            .sort_by(|a, b| a.partial_cmp(b).expect("durations never produce NaN nanos"));

        let to_duration = |v: f64| Duration::from_nanos(v.round() as u64);
        DurationDistribution {
            samples: self.0.len(),
            mean: to_duration(mean),
            median: to_duration(percentile(&self.0, 1, 2)), // 1/2 = 0.50 = p50
            p95: to_duration(percentile(&self.0, 19, 20)),  // 19/20 = 0.95 = p95
            p99: to_duration(percentile(&self.0, 99, 100)), // 99/100 = 0.99 = p99
            std_deviation: to_duration(std_deviation),
        }
    }
}

fn percentile(sorted: &[f64], num: u32, den: u32) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let last = (sorted.len() - 1) as u32;
    let scaled = u64::from(num) * u64::from(last);
    let rank = (scaled / u64::from(den)) as u32;
    let rem = scaled % u64::from(den);

    let lower = sorted[rank as usize];
    if rem == 0 || rank == last {
        return lower;
    }
    let upper = sorted[rank as usize + 1];
    let frac = rem as f64 / f64::from(den);
    lower + (upper - lower) * frac
}

pub struct DurationDistribution {
    /// number of samples the distribution was computed from.
    pub samples: usize,
    /// arithmetic mean.
    pub mean: Duration,
    /// 50th percentile (linearly interpolated between the two middle ranks for even sample
    /// counts).
    pub median: Duration,
    /// population standard deviation (divides by `n`, not `n - 1` — the full sample set is
    /// known, not estimated from a subset).
    pub std_deviation: Duration,
    /// 95th percentile.
    pub p95: Duration,
    /// 99th percentile.
    pub p99: Duration,
}

/// Formats a duration with a single appropriate unit and a few significant digits (e.g.
/// `989.269ms`), instead of `humantime`'s full breakdown (`989ms 268us 753ns`) — the latter
/// reads fine as a one-off but becomes very dense once repeated across many stats and cells.
fn format_duration(d: Duration) -> String {
    let nanos = d.as_nanos() as f64;
    let (value, unit) = if nanos >= 1_000_000_000.0 {
        (nanos / 1_000_000_000.0, "s")
    } else if nanos >= 1_000_000.0 {
        (nanos / 1_000_000.0, "ms")
    } else if nanos >= 1_000.0 {
        (nanos / 1_000.0, "us")
    } else {
        (nanos, "ns")
    };
    format!("{value:.3}{unit}")
}

impl Display for DurationDistribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // mean ± stddev up front (the usual benchmark-report convention), percentiles after —
        // still visible, but not competing with the headline number for attention.
        write!(
            f,
            "{} ± {} (median={}, p95={}, p99={}, n={})",
            format_duration(self.mean),
            format_duration(self.std_deviation),
            format_duration(self.median),
            format_duration(self.p95),
            format_duration(self.p99),
            self.samples,
        )
    }
}

pub struct LatencyDistribution {
    /// end-to-end latency distribution, from t=0 (either the external dispatch time passed to
    /// [`Latency::new_with_t0`], or the earliest span start when using [`Latency::new`]) to the
    /// last span's end.
    pub full: DurationDistribution,
    /// per-cell latency distributions, keyed by the SRI of the cell that produced them.
    pub cells: BTreeMap<Sri, CellLatencyDistribution>,
}

impl Display for LatencyDistribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "full latency: {}", self.full)?;
        for (sri, dist) in &self.cells {
            writeln!(f, "  {sri}:")?;
            write!(f, "{dist}")?;
        }

        Ok(())
    }
}

pub struct CellLatencyDistribution {
    /// distribution of this cell's start time, offset from the call's t=0.
    pub starts: DurationDistribution,
    /// distribution of this cell's end time, offset from the call's t=0.
    pub ends: DurationDistribution,
    /// distribution of this cell's total handling time (`end - start`).
    pub durations: DurationDistribution,
}

impl Display for CellLatencyDistribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "    start:    {}", self.starts)?;
        writeln!(f, "    end:      {}", self.ends)?;
        writeln!(f, "    duration: {}", self.durations)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use swarm_telemetry::db::opentelemetry_proto::tonic::common::v1::{
        AnyValue, KeyValue, any_value,
    };

    use super::*;

    fn span(name: &str, start: u64, end: u64) -> Span {
        Span {
            attributes: vec![KeyValue {
                key: "sri".to_owned(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue(test_sri(name).to_string())),
                }),
                key_strindex: 0,
            }],
            start_time_unix_nano: start,
            end_time_unix_nano: end,
            ..Default::default()
        }
    }

    fn test_sri(name: &str) -> Sri {
        Sri::of_path(name).unwrap()
    }

    #[test]
    fn latency_uses_earliest_tagged_span_start_as_t0() {
        let spans = vec![span("a", 100, 150), span("b", 150, 220)];

        let latency = Latency::new(&spans);

        assert_eq!(latency.full, Duration::from_nanos(120));
        assert_eq!(latency.cells.len(), 2);
        assert_eq!(latency.cells[0].sri, test_sri("a"));
        assert_eq!(latency.cells[0].offset, Duration::ZERO);
        assert_eq!(latency.cells[0].duration, Duration::from_nanos(50));
        assert_eq!(latency.cells[1].offset, Duration::from_nanos(50));
        assert_eq!(latency.cells[1].duration, Duration::from_nanos(70));
    }

    #[test]
    fn latency_ignores_spans_without_an_sri() {
        let mut untagged = span("irrelevant", 0, 10);
        untagged.attributes.clear();
        let spans = vec![untagged, span("a", 10, 40)];

        let latency = Latency::new(&spans);

        // t0 is only ever seeded from a span carrying an SRI, so the untagged span's earlier
        // start must not shift the offsets of the tagged one.
        assert_eq!(latency.cells.len(), 1);
        assert_eq!(latency.cells[0].offset, Duration::ZERO);
        assert_eq!(latency.full, Duration::from_nanos(30));
    }

    #[test]
    fn latency_with_external_t0_offsets_from_the_given_time() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let spans = vec![span(
            "a",
            nanos_since_epoch(t0) + 50,
            nanos_since_epoch(t0) + 90,
        )];

        let latency = Latency::new_with_t0(&spans, t0);

        assert_eq!(latency.cells[0].offset, Duration::from_nanos(50));
        assert_eq!(latency.cells[0].duration, Duration::from_nanos(40));
        assert_eq!(latency.full, Duration::from_nanos(90));
    }

    fn nanos_since_epoch(t: SystemTime) -> u64 {
        t.duration_since(UNIX_EPOCH)
            .expect("test time must be after the epoch")
            .as_nanos()
            .try_into()
            .expect("test time fits in a u64 nanosecond count")
    }

    #[test]
    fn distribution_of_single_sample_equals_that_sample_everywhere() {
        let mut collection = DurationCollection::default();
        collection.push(42.0);

        let distribution = collection.distribution();

        assert_eq!(distribution.mean, Duration::from_nanos(42));
        assert_eq!(distribution.median, Duration::from_nanos(42));
        assert_eq!(distribution.p95, Duration::from_nanos(42));
        assert_eq!(distribution.p99, Duration::from_nanos(42));
        assert_eq!(distribution.std_deviation, Duration::ZERO);
    }

    #[test]
    fn distribution_computes_mean_and_median_of_a_known_set() {
        let mut collection = DurationCollection::default();
        for v in [10.0, 20.0, 30.0, 40.0] {
            collection.push(v);
        }

        let distribution = collection.distribution();

        assert_eq!(distribution.mean, Duration::from_nanos(25));
        // linear interpolation between the two middle values (20, 30) at rank 1.5
        assert_eq!(distribution.median, Duration::from_nanos(25));
    }

    #[test]
    fn distribution_std_deviation_matches_population_formula() {
        let mut collection = DurationCollection::default();
        for v in [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0] {
            collection.push(v);
        }

        let distribution = collection.distribution();

        // population variance of this classic example set is 4, so std_dev is 2
        assert_eq!(distribution.std_deviation, Duration::from_nanos(2));
    }

    #[test]
    fn percentile_of_single_element_slice_is_that_element() {
        assert!((percentile(&[7.0], 99, 100) - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn percentile_interpolates_between_neighbouring_ranks() {
        let sorted = [0.0, 10.0, 20.0, 30.0];

        // p99 -> rank = 0.99 * 3 = 2.97, between index 2 (20.0) and 3 (30.0)
        let p99 = percentile(&sorted, 99, 100);
        assert!((p99 - 29.7).abs() < 1e-9);
    }

    #[test]
    fn latency_collection_groups_cell_measurements_by_sri() {
        let run_a = Latency::new(&[span("a", 0, 10), span("b", 10, 30)]);
        let run_b = Latency::new(&[span("a", 0, 20), span("b", 20, 50)]);

        let collection: LatencyCollection = vec![run_a, run_b].into_iter().collect();
        let distribution = collection.distribution();

        assert_eq!(distribution.cells.len(), 2);
        let a = &distribution.cells[&test_sri("a")];
        // durations for "a" across both runs were 10 and 20 -> mean 15
        assert_eq!(a.durations.mean, Duration::from_nanos(15));
    }
}
