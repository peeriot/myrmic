//! this module is collecting metrics for nodes. currently bound to the introspection plugin but
//! could evolve into a separate library.
//!
//! for the health observability of nodes we are interested in CPU, memory and disk usage. to get
//! a complete picture we will monitor the values for the OS the node is running on and specifically
//! the process of the node.
//!
//! The metrics pipeline has three layers:
//! - sampling raw values from [`sysinfo::System`]
//! - normalizing them into [`data_point::DataPoint`] values that carry both the latest sample and
//!   the delta to the previous sample
//! - publishing normalized values through [`publish::PublishNodeMetrics`]
//!
//! Some metrics are modeled as gauges and may move up or down between samples, while cumulative
//! counters are modeled as monotonically increasing and can fail refresh if they decrease.

use std::sync::Arc;

use sysinfo::{CpuRefreshKind, MemoryRefreshKind, ProcessRefreshKind, RefreshKind, System};
use tokio::sync::oneshot::Receiver;

use self::{
    data_point::MonotonicError,
    process::ProcessMetrics,
    publish::{NodeMetric, NoopPublisher, PublishNodeMetrics},
    system::SystemMetrics,
};

pub(crate) mod data_point;
mod process;
pub(crate) mod publish;
mod system;

/// Holds the local sampling state for node metrics.
///
/// [`NodeMetrics`] owns the [`sysinfo::System`] snapshot cache, the current process id used for
/// process-local metrics, and a publisher that receives normalized output. A refresh mutates the
/// cached metric state; a publish emits the already refreshed state.
///
/// It requires a publisher to transmit metrics to whatever destination, e.g.:
/// - `NoOp`
/// - `OpenTelemetry`
pub(crate) struct NodeMetrics {
    /// Which categories of system information to refresh on each tick.
    refresh_kind: RefreshKind,
    /// The sysinfo snapshot cache; updated in-place on each refresh.
    sys: System,
    /// Sink that receives normalized metrics after a successful refresh.
    publisher: Arc<dyn PublishNodeMetrics + Send + Sync>,
    /// PID of this process, used to look up process-local statistics.
    pid: Option<sysinfo::Pid>,
    /// Latest sampled data points for OS-wide metrics.
    system_metrics: system::SystemMetrics,
    /// Latest sampled data points for this process.
    process_metrics: process::ProcessMetrics,
}

impl NodeMetrics {
    /// Creates a [`NodeMetrics`] with the given publisher, using default refresh settings.
    // unused until real publisher is implemented
    pub fn new(publisher: Arc<dyn PublishNodeMetrics + Send + Sync>) -> Self {
        Self {
            publisher,
            ..Default::default()
        }
    }
}

impl Default for NodeMetrics {
    /// A default [`NodeMetrics`] uses the [`NoopPublisher`].
    ///
    /// The configured [`RefreshKind`] only enables the data that this module currently publishes:
    /// CPU, memory, and process disk statistics.
    fn default() -> Self {
        let refresh_kind = RefreshKind::nothing()
            // we want the CPU usage of the system
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
            // we want the memory usage of the system
            .with_memory(MemoryRefreshKind::everything())
            // we want information about processes
            .with_processes(
                ProcessRefreshKind::nothing()
                    // we want process CPU usage
                    .with_cpu()
                    // we want process memory usage
                    .with_memory()
                    // we want process disk usage
                    .with_disk_usage(),
            );

        Self {
            sys: System::new_with_specifics(refresh_kind),
            refresh_kind,
            pid: sysinfo::get_current_pid().ok(),
            system_metrics: SystemMetrics::default(),
            process_metrics: ProcessMetrics::default(),
            publisher: Arc::new(NoopPublisher),
        }
    }
}

impl NodeMetrics {
    /// Refreshes system and process metrics from [`sysinfo::System`].
    ///
    /// This updates the cached metric values in-place. A monotonicity failure in one of the
    /// cumulative process counters aborts the refresh for that tick and leaves publication to the
    /// caller.
    fn refresh(&mut self) -> Result<(), MonotonicError> {
        // refresh the system info
        self.sys.refresh_specifics(self.refresh_kind);

        // extract system data points
        self.system_metrics.extract(&self.sys);

        if let Some(process) = self.pid.and_then(|pid| self.sys.process(pid)) {
            // extract process data points
            self.process_metrics.extract(process)?;
        }

        Ok(())
    }

    /// Publishes the most recently refreshed metrics through the configured publisher.
    ///
    /// This method does not refresh values on its own; callers are expected to invoke it only
    /// after a successful [`Self::refresh`].
    fn publish(&self) {
        self.publisher.publish_metric(NodeMetric::SystemCpuUsage(
            &self.system_metrics.global_cpu_usage,
        ));
        self.publisher.publish_metric(NodeMetric::SystemUsedMemory(
            &self.system_metrics.used_memory,
        ));
        self.publisher.publish_metric(NodeMetric::SystemTotalMemory(
            &self.system_metrics.total_memory,
        ));

        self.publisher
            .publish_metric(NodeMetric::ProcessCpuUsage(&self.process_metrics.cpu_usage));
        self.publisher
            .publish_metric(NodeMetric::ProcessMemory(&self.process_metrics.memory));
        self.publisher
            .publish_metric(NodeMetric::ProcessVirtualMemory(
                &self.process_metrics.virtual_memory,
            ));
        self.publisher
            .publish_metric(NodeMetric::ProcessDiskTotalRead(
                &self.process_metrics.total_read_bytes,
            ));
        self.publisher
            .publish_metric(NodeMetric::ProcessDiskTotalWritten(
                &self.process_metrics.total_written_bytes,
            ));
    }
}

/// Collect metrics on a fixed interval until a shutdown signal is received.
///
/// On every tick this function refreshes the cached metrics and publishes them only if refresh
/// succeeds. A monotonicity violation causes the whole tick to be skipped and logged. Shutdown is
/// cooperative through the provided oneshot receiver.
pub(crate) async fn collect(
    mut metrics: NodeMetrics,
    mut interval: tokio::time::Interval,
    mut poison_rcv: Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = interval.tick() => {
                // spawn blocking to no block tokio runtime
                let result = tokio::task::spawn_blocking(move || {
                    // refresh system metrics and extract data points
                    let result = metrics.refresh();
                        (metrics, result)
                }).await;

                match result {
                    Ok((updated, Ok(()))) => {
                        metrics = updated;
                        metrics.publish();
                    }
                    Ok((updated, Err(_))) => {
                        metrics = updated;
                        tracing::error!("A monotonically increasing value decreased! Node metric update ignored!");
                    }
                    Err(e) => {
                        tracing::error!("Failed to spawn metric collection: {e}");
                        break;
                    }
                }
            }
            // Shutdown signal
            _ = &mut poison_rcv => {
                break;
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::{sync::Arc, time::Duration};

    use opentelemetry::{InstrumentationScope, global};
    use opentelemetry_sdk::metrics::{
        InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        data::{
            AggregatedMetrics, GaugeDataPoint, Metric, MetricData, ResourceMetrics, ScopeMetrics,
            SumDataPoint,
        },
    };

    /// Finds the most recently exported [`Metric`] with the given name, scanning from the newest
    /// [`ResourceMetrics`] batch first. Panics if no match is found.
    fn named_metric<'a>(resource_metrics: &'a [ResourceMetrics], name: &str) -> &'a Metric {
        resource_metrics
            .iter()
            .rev()
            .flat_map(ResourceMetrics::scope_metrics)
            .flat_map(ScopeMetrics::metrics)
            .find(|metric| metric.name() == name)
            .unwrap_or_else(|| panic!("missing metric {name}"))
    }

    /// Extracts the first data point from an `f64` gauge metric. Panics if the metric is not a
    /// gauge or has no data points.
    fn gauge_data_point(metric: &Metric) -> &GaugeDataPoint<f64> {
        match metric.data() {
            AggregatedMetrics::F64(MetricData::Gauge(gauge)) => {
                gauge.data_points().next().expect("gauge data point")
            }
            other => panic!("expected gauge f64 but found: {other:?}"),
        }
    }

    /// Extracts the first data point from an `i64` sum (up-down counter) metric. Panics if the
    /// metric is not an `i64` sum or has no data points.
    fn updown_counter(metric: &Metric) -> &SumDataPoint<i64> {
        match metric.data() {
            AggregatedMetrics::I64(MetricData::Sum(sum)) => {
                sum.data_points().next().expect("sum data point")
            }
            other => panic!("expected sum i64 but found: {other:?}"),
        }
    }

    /// Wires up an [`InMemoryMetricExporter`] with a long periodic-reader interval so that only
    /// explicit [`SdkMeterProvider::force_flush`] calls trigger exports during tests.
    fn setup_open_telemetry() -> (InMemoryMetricExporter, SdkMeterProvider) {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(exporter.clone())
                    .with_interval(Duration::from_mins(1))
                    .build(),
            )
            .build();
        global::set_meter_provider(provider.clone());
        (exporter, provider)
    }

    /// Verifies the full collection-and-publish pipeline against a real in-process sysinfo
    /// snapshot exported to an in-memory OpenTelemetry sink.
    ///
    /// The test runs two publish cycles:
    /// 1. **Before refresh** — all data points are at their zero-initialized defaults.
    /// 2. **After refresh** — data points reflect the live system and must be positive.
    ///
    /// `process.disk.io` is intentionally not asserted after refresh: sysinfo may return zero on
    /// the first sample because the OS caches disk counters and they are not always available
    /// immediately.
    #[test]
    fn node_metrics_collection() {
        let (exporter, provider) = setup_open_telemetry();

        let publisher = Arc::new(super::publish::OtelPublisher::new(
            InstrumentationScope::builder("node-metrics-test").build(),
        ));
        let mut metrics = super::NodeMetrics::new(publisher);

        // Publish the zero-initialized state before any refresh and flush to capture the export.
        metrics.publish();
        provider.force_flush().unwrap();

        let exported = exporter.get_finished_metrics().unwrap();
        assert_eq!(1, exported.len());

        // All values should be zero before the first refresh.
        assert!(
            gauge_data_point(named_metric(&exported, "system.cpu.utilization"))
                .value()
                .abs()
                < 0.1
        );
        assert_eq!(
            0,
            updown_counter(named_metric(&exported, "system.memory.usage")).value()
        );
        assert_eq!(
            0,
            updown_counter(named_metric(&exported, "system.memory.limit")).value()
        );
        assert!(
            gauge_data_point(named_metric(&exported, "process.cpu.utilization"))
                .value()
                .abs()
                < 0.1
        );
        assert_eq!(
            0,
            updown_counter(named_metric(&exported, "process.memory.usage")).value()
        );
        assert_eq!(
            0,
            updown_counter(named_metric(&exported, "process.memory.virtual")).value()
        );

        // Refresh from sysinfo, publish the live values, and flush a second batch.
        metrics.refresh().unwrap();
        metrics.publish();
        provider.force_flush().unwrap();

        let exported = exporter.get_finished_metrics().unwrap();
        assert_eq!(2, exported.len());

        // All values should be positive after a real sysinfo refresh.
        assert!(0.0 < gauge_data_point(named_metric(&exported, "system.cpu.utilization")).value());
        assert!(0 < updown_counter(named_metric(&exported, "system.memory.usage")).value());
        assert!(0 < updown_counter(named_metric(&exported, "system.memory.limit")).value());
        assert!(0.0 < gauge_data_point(named_metric(&exported, "process.cpu.utilization")).value());
        assert!(0 < updown_counter(named_metric(&exported, "process.memory.usage")).value());
        assert!(0 < updown_counter(named_metric(&exported, "process.memory.virtual")).value());
        // process.disk.io is not asserted here: sysinfo may return zero on the first sample
        // because the OS caches disk counters and they are not always immediately available.

        provider.shutdown().unwrap();
    }
}
