use sysinfo::System;

use super::data_point::{GaugeF32, NonMonotonicCounter};

/// Holds sampled data points for operating-system-wide metrics.
#[derive(Default, Debug)]
pub(crate) struct SystemMetrics {
    /// "global" CPUs usage (aka the addition of all the CPUs).
    pub(crate) global_cpu_usage: GaugeF32,
    /// The amount of free RAM in bytes
    pub(crate) free_memory: NonMonotonicCounter,
    /// The amount of used RAM in bytes.
    pub(crate) used_memory: NonMonotonicCounter,
    /// The total amount of RAM in bytes.
    pub(crate) total_memory: NonMonotonicCounter,
}

impl SystemMetrics {
    /// Extract gauge-like metrics from a refreshed [`sysinfo::System`].
    ///
    /// This is infallible because all tracked system metrics in this struct are modeled as gauges,
    /// not monotonic counters.
    pub(crate) fn extract(&mut self, sys: &System) {
        self.global_cpu_usage.update_value(sys.global_cpu_usage());
        self.used_memory.update_value(sys.used_memory());
        self.free_memory.update_value(sys.free_memory());
        self.total_memory.update_value(sys.total_memory());
    }
}
