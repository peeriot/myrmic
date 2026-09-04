use super::data_point::DataPoint;

mod noop;
mod otel;

pub(crate) use noop::NoopPublisher;
pub(crate) use otel::OtelPublisher;

/// A normalized node metric ready to be handed to a publisher.
///
/// Each variant exposes a [`DataPoint`], which contains both the current absolute value and the
/// delta to the previous sample. Variants are split between system-wide and current-process
/// metrics.
// unused until real publisher is implemented
pub enum NodeMetric<'dp> {
    /// Aggregate CPU utilization across all cores, as a fraction in `[0.0, 1.0]`.
    SystemCpuUsage(&'dp DataPoint<f32>),
    /// Amount of RAM actively used by the OS, in bytes.
    SystemUsedMemory(&'dp DataPoint<u64, i64>),
    /// Total installed RAM, in bytes.
    SystemTotalMemory(&'dp DataPoint<u64, i64>),
    /// CPU utilization of this process across all cores, as a fraction in `[0.0, 1.0]`.
    ProcessCpuUsage(&'dp DataPoint<f32>),
    /// Resident (physical) memory used by this process, in bytes.
    ProcessMemory(&'dp DataPoint<u64, i64>),
    /// Virtual memory mapped by this process, in bytes.
    ProcessVirtualMemory(&'dp DataPoint<u64, i64>),
    /// Cumulative bytes read from disk by this process since it started.
    ProcessDiskTotalRead(&'dp DataPoint<u64>),
    /// Cumulative bytes written to disk by this process since it started.
    ProcessDiskTotalWritten(&'dp DataPoint<u64>),
}

/// Sink for normalized node metrics.
///
/// Publishers are called synchronously from the collection loop after a successful refresh. The
/// current implementation assumes publication is cheap and non-blocking enough to run inline.
pub(crate) trait PublishNodeMetrics {
    fn publish_metric(&self, metric: NodeMetric<'_>);
}
