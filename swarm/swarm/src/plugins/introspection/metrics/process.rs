use sysinfo::Process;

use super::data_point::{GaugeF32, MonotonicCounter, MonotonicError, NonMonotonicCounter};

/// Holds sampled data points for the current process.
#[derive(Default, Debug)]
pub(crate) struct ProcessMetrics {
    /// The total CPU usage (in %). Notice that it might be bigger than 100 if run on a multi-core
    /// machine.
    pub(crate) cpu_usage: GaugeF32,
    /// The memory usage (in bytes)
    pub(crate) memory: NonMonotonicCounter,
    /// The virtual memory usage (in bytes)
    pub(crate) virtual_memory: NonMonotonicCounter,
    /// Total number of read bytes.
    pub(crate) total_read_bytes: MonotonicCounter,
    /// Total number of written bytes.
    pub(crate) total_written_bytes: MonotonicCounter,
    /// Number of read bytes since the last extract.
    pub(crate) read_bytes: NonMonotonicCounter,
    /// Number of written bytes since the last extract.
    pub(crate) written_bytes: NonMonotonicCounter,
}

impl ProcessMetrics {
    /// Extract metrics from a refreshed [`sysinfo::Process`].
    ///
    /// Most process metrics here are gauges and therefore update infallibly. The only fallible
    /// updates are the cumulative disk counters, which are modeled as monotonically increasing and
    /// return [`MonotonicError`] if the source value decreases.
    pub(crate) fn extract(&mut self, process: &Process) -> Result<(), MonotonicError> {
        self.cpu_usage.update_value(process.cpu_usage());

        self.memory.update_value(process.memory());
        self.virtual_memory.update_value(process.virtual_memory());

        let disk_usage = process.disk_usage();
        self.total_read_bytes
            .update_value(disk_usage.total_read_bytes)?;
        self.total_written_bytes
            .update_value(disk_usage.total_written_bytes)?;

        self.read_bytes.update_value(disk_usage.read_bytes);
        self.written_bytes.update_value(disk_usage.written_bytes);

        Ok(())
    }
}
