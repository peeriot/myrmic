use opentelemetry::{
    InstrumentationScope, KeyValue,
    metrics::{Counter, Gauge, UpDownCounter},
};

// the system metric names are aligned to:
// https://opentelemetry.io/docs/specs/semconv/system/system-metrics/
// those not described in OpenTelemetry are aligned with related metrics
const SYSTEM_CPU_UTILIZATION: &str = "system.cpu.utilization";
const SYSTEM_MEMORY_USAGE: &str = "system.memory.usage";
const SYSTEM_MEMORY_LIMIT: &str = "system.memory.limit";

// the process metric names are aligned to:
// https://opentelemetry.io/docs/specs/semconv/system/process-metrics/
const PROCESS_CPU_UTILIZATION: &str = "process.cpu.utilization";
const PROCESS_MEMORY_USAGE: &str = "process.memory.usage";
const PROCESS_MEMORY_VIRTUAL: &str = "process.memory.virtual";
const PROCESS_DISK_IO: &str = "process.disk.io";

const UNIT_BYTES: &str = "By";

/// OpenTelemetry implementation of [`super::PublishNodeMetrics`].
///
/// Instrument names follow the OpenTelemetry semantic conventions:
/// - System metrics: <https://opentelemetry.io/docs/specs/semconv/system/system-metrics/>
/// - Process metrics: <https://opentelemetry.io/docs/specs/semconv/system/process-metrics/>
///
/// The instruments are registered against a [`opentelemetry::metrics::Meter`] obtained from the
/// global meter provider using the [`InstrumentationScope`] passed to [`OtelPublisher::new`].
pub(crate) struct OtelPublisher {
    system_cpu_utilization: Gauge<f64>,
    system_memory_usage: UpDownCounter<i64>,
    system_memory_limit: UpDownCounter<i64>,
    process_cpu_utilization: Gauge<f64>,
    process_memory_usage: UpDownCounter<i64>,
    process_memory_virtual: UpDownCounter<i64>,
    process_disk_io: Counter<u64>,
    process_id: u32,
}

impl OtelPublisher {
    /// Creates an [`OtelPublisher`] and registers all metric instruments with the global meter
    /// provider under the given `scope`.
    pub(crate) fn new(scope: InstrumentationScope) -> Self {
        let meter = opentelemetry::global::meter_with_scope(scope);

        Self {
            system_cpu_utilization: meter.f64_gauge(SYSTEM_CPU_UTILIZATION).build(),
            system_memory_usage: meter
                .i64_up_down_counter(SYSTEM_MEMORY_USAGE)
                .with_unit(UNIT_BYTES)
                .build(),
            system_memory_limit: meter
                .i64_up_down_counter(SYSTEM_MEMORY_LIMIT)
                .with_unit(UNIT_BYTES)
                .build(),
            process_cpu_utilization: meter.f64_gauge(PROCESS_CPU_UTILIZATION).build(),
            process_memory_usage: meter
                .i64_up_down_counter(PROCESS_MEMORY_USAGE)
                .with_unit(UNIT_BYTES)
                .build(),
            process_memory_virtual: meter
                .i64_up_down_counter(PROCESS_MEMORY_VIRTUAL)
                .with_unit(UNIT_BYTES)
                .build(),
            process_disk_io: meter
                .u64_counter(PROCESS_DISK_IO)
                .with_unit(UNIT_BYTES)
                .build(),
            process_id: std::process::id(),
        }
    }
}

impl super::PublishNodeMetrics for OtelPublisher {
    fn publish_metric(&self, metric: super::NodeMetric<'_>) {
        let pid = KeyValue::new("pid", self.process_id.to_string());
        match metric {
            super::NodeMetric::SystemCpuUsage(data_point) => {
                // sysinfo provides CPU usage in the 0 to 100 range while OTel expects 0 to 1
                self.system_cpu_utilization
                    .record(Into::<f64>::into(data_point.value()) / 100.0f64, &[]);
            }
            super::NodeMetric::SystemUsedMemory(data_point) => {
                self.system_memory_usage.add(data_point.last_offset(), &[]);
            }
            super::NodeMetric::SystemTotalMemory(data_point) => {
                self.system_memory_limit.add(data_point.last_offset(), &[]);
            }
            super::NodeMetric::ProcessCpuUsage(data_point) => {
                // sysinfo provides CPU usage in the 0 to 100 range while OTel expects 0 to 1
                self.process_cpu_utilization
                    .record(Into::<f64>::into(data_point.value()) / 100.0f64, &[pid]);
            }
            super::NodeMetric::ProcessMemory(data_point) => {
                self.process_memory_usage
                    .add(data_point.last_offset(), &[pid]);
            }
            super::NodeMetric::ProcessVirtualMemory(data_point) => {
                self.process_memory_virtual
                    .add(data_point.last_offset(), &[pid]);
            }
            super::NodeMetric::ProcessDiskTotalRead(data_point) => {
                self.process_disk_io.add(
                    data_point.last_offset(),
                    &[KeyValue::new("disk.io.direction", "read"), pid],
                );
            }
            super::NodeMetric::ProcessDiskTotalWritten(data_point) => {
                self.process_disk_io.add(
                    data_point.last_offset(),
                    &[KeyValue::new("disk.io.direction", "write"), pid],
                );
            }
        }
    }
}
