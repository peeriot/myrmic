//! The structs defining the configuration of the exec plugin live in common, since they also need to be known to
//! the orchestration plugins (they need to know about exec plugin presence and configuration from the info provided
//! by the plugin system).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CapabilityTag, Result, bail, custom_err};

const DEFAULT_INIT_TIMEOUT: Duration = Duration::from_secs(10); // same as zenoh's default
const DEFAULT_FUEL: u64 = 1_000_000_000;
const DEFAULT_CELL_MEMORY: u64 = 16 * 1024 * 1024;
// Tried at 1_000_000 on 2026-09-01 (a2213793) chasing the ~1.4-7ms dispatch
// wall around 13µs handlers, on the theory that yields every ~1000 operators
// cost a run-queue turn each. Run 33463140540 came back identical to the
// 1000-interval run at every load, so fuel yields are not where that time
// goes; reverted.
const DEFAULT_FUEL_YIELD_INTERVAL: u64 = 1000;
// Best-effort backstop; promptness comes from the mailbox event subscription.
const DEFAULT_MAILBOX_POLL_INTERVAL: Duration = Duration::from_secs(5);
// 8 until 2026-08-29: with the multi-round-trip mailbox transactions of the
// time, per-event db cost dominated and batching measurably did not amortize
// (rack run 33182700124 — batch duration grew with batch size). The one-shot
// mailbox ops changed that: a poll now costs two zenoh queries however many
// entries it returns, so a deeper batch amortizes the poll instead of
// stretching it, and 8 hard-capped a saturated fan-in cell at ~8 events per
// ~5 ms cycle.
const DEFAULT_MAILBOX_BATCH_SIZE: usize = 64;
const DEFAULT_EVENT_BUFFER_SIZE: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "ConfigTemplate")]
pub struct Config {
    name: Option<String>,
    runner_fuel: Option<u64>,
    runner_fuel_max: Option<u64>,
    cell_memory: Option<u64>,
    cell_memory_max: Option<u64>,
    fuel_yield_interval: Option<u64>,
    init_timeout: Option<Duration>,
    mailbox_poll_interval: Option<Duration>,
    mailbox_batch_size: Option<usize>,
    event_buffer_size: Option<usize>,
    tags: Vec<CapabilityTag>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: None,
            runner_fuel: Some(DEFAULT_FUEL),
            runner_fuel_max: None,
            cell_memory: Some(DEFAULT_CELL_MEMORY),
            cell_memory_max: None,
            fuel_yield_interval: Some(DEFAULT_FUEL_YIELD_INTERVAL),
            init_timeout: None,
            mailbox_poll_interval: Some(DEFAULT_MAILBOX_POLL_INTERVAL),
            mailbox_batch_size: Some(DEFAULT_MAILBOX_BATCH_SIZE),
            event_buffer_size: Some(DEFAULT_EVENT_BUFFER_SIZE),
            tags: vec![],
        }
    }
}

impl Config {
    pub fn try_from_value(value: &Value) -> Result<Self> {
        let config_template: ConfigTemplate = serde_json::from_value(value.clone())
            .map_err(|err| custom_err!("failed to deserialize exec plugin config: {err}"))?;
        config_template.try_into()
    }

    fn check_limits(&self) -> Result<()> {
        if self.runner_fuel() > self.runner_fuel_max() {
            bail!(
                "execution.runner_fuel ({}) exceeds execution.runner_fuel_max ({})",
                self.runner_fuel(),
                self.runner_fuel_max()
            );
        }
        if self.cell_memory() > self.cell_memory_max() {
            bail!(
                "execution.cell_memory ({}) exceeds execution.cell_memory_max ({})",
                self.cell_memory(),
                self.cell_memory_max()
            );
        }

        Ok(())
    }

    pub fn set_capability_tags(&mut self, tags: Vec<CapabilityTag>) -> &mut Self {
        self.tags = tags;
        self
    }

    pub fn set_runner_fuel(&mut self, fuel: u64) -> &mut Self {
        self.runner_fuel = Some(fuel);
        self
    }

    fn set_runner_fuel_max(&mut self, fuel: u64) -> &mut Self {
        self.runner_fuel_max = Some(fuel);
        self
    }

    fn set_cell_memory(&mut self, bytes: u64) -> &mut Self {
        self.cell_memory = Some(bytes);
        self
    }

    fn set_cell_memory_max(&mut self, bytes: u64) -> &mut Self {
        self.cell_memory_max = Some(bytes);
        self
    }

    pub fn set_fuel_yield_interval(&mut self, interval: u64) -> &mut Self {
        self.fuel_yield_interval = Some(interval);
        self
    }

    pub fn set_init_timeout(&mut self, init_timeout: Duration) -> &mut Self {
        self.init_timeout = Some(init_timeout);
        self
    }

    pub fn set_mailbox_poll_interval(&mut self, interval: Duration) -> &mut Self {
        self.mailbox_poll_interval = Some(interval);
        self
    }

    pub fn set_mailbox_batch_size(&mut self, size: usize) -> &mut Self {
        self.mailbox_batch_size = Some(size);
        self
    }

    pub fn set_event_buffer_size(&mut self, size: usize) -> &mut Self {
        self.event_buffer_size = Some(size);
        self
    }

    pub fn set_name(&mut self, name: String) -> &mut Self {
        self.name = Some(name);
        self
    }

    #[must_use]
    pub fn capability_tags(&self) -> &[CapabilityTag] {
        &self.tags
    }

    pub fn take_capability_tags(&mut self) -> Vec<CapabilityTag> {
        std::mem::take(&mut self.tags)
    }

    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[must_use]
    pub fn init_timeout(&self) -> Duration {
        self.init_timeout.unwrap_or(DEFAULT_INIT_TIMEOUT)
    }

    #[must_use]
    pub fn runner_fuel(&self) -> u64 {
        self.runner_fuel.unwrap_or(DEFAULT_FUEL)
    }

    #[must_use]
    pub fn runner_fuel_max(&self) -> u64 {
        self.runner_fuel_max.unwrap_or_else(|| self.runner_fuel())
    }

    #[must_use]
    pub fn cell_memory(&self) -> u64 {
        self.cell_memory.unwrap_or(DEFAULT_CELL_MEMORY)
    }

    #[must_use]
    pub fn cell_memory_max(&self) -> u64 {
        self.cell_memory_max.unwrap_or_else(|| self.cell_memory())
    }

    #[must_use]
    pub fn fuel_yield_interval(&self) -> u64 {
        self.fuel_yield_interval
            .unwrap_or(DEFAULT_FUEL_YIELD_INTERVAL)
    }

    #[must_use]
    pub fn mailbox_poll_interval(&self) -> Duration {
        self.mailbox_poll_interval
            .unwrap_or(DEFAULT_MAILBOX_POLL_INTERVAL)
    }

    #[must_use]
    pub fn mailbox_batch_size(&self) -> usize {
        self.mailbox_batch_size
            .unwrap_or(DEFAULT_MAILBOX_BATCH_SIZE)
    }

    #[must_use]
    pub fn event_buffer_size(&self) -> usize {
        self.event_buffer_size.unwrap_or(DEFAULT_EVENT_BUFFER_SIZE)
    }
}

/// Defines the template of the json file defining the configuration of the exec plugin
#[derive(Deserialize)]
struct ConfigTemplate {
    #[serde(alias = "max_fuel_per_handler")]
    runner_fuel: Option<u64>,
    runner_fuel_max: Option<u64>,
    cell_memory: Option<u64>,
    cell_memory_max: Option<u64>,
    fuel_yield_interval: Option<u64>,
    init_timeout_secs: Option<f64>,
    mailbox_poll_interval_ms: Option<u64>,
    mailbox_batch_size: Option<usize>,
    event_buffer_size: Option<usize>,
    tags: Option<Vec<String>>,
    name: Option<String>,
}

impl TryFrom<ConfigTemplate> for Config {
    type Error = crate::Error;

    fn try_from(config: ConfigTemplate) -> Result<Self> {
        let mut exec_config = Config::default();

        if let Some(fuel_yield_interval) = config.fuel_yield_interval {
            exec_config.set_fuel_yield_interval(fuel_yield_interval);
        }
        if let Some(runner_fuel) = config.runner_fuel {
            exec_config.set_runner_fuel(runner_fuel);
        }
        if let Some(fuel_max) = config.runner_fuel_max {
            exec_config.set_runner_fuel_max(fuel_max);
        }
        if let Some(bytes) = config.cell_memory {
            exec_config.set_cell_memory(bytes);
        }
        if let Some(bytes) = config.cell_memory_max {
            exec_config.set_cell_memory_max(bytes);
        }
        if let Some(init_timeout_secs) = config.init_timeout_secs {
            exec_config.set_init_timeout(Duration::from_secs_f64(init_timeout_secs));
        }
        if let Some(name) = config.name {
            exec_config.set_name(name);
        }
        if let Some(ms) = config.mailbox_poll_interval_ms {
            exec_config.set_mailbox_poll_interval(Duration::from_millis(ms));
        }
        if let Some(size) = config.mailbox_batch_size {
            exec_config.set_mailbox_batch_size(size);
        }
        if let Some(size) = config.event_buffer_size {
            exec_config.set_event_buffer_size(size);
        }
        if let Some(tags) = config.tags {
            let capa_tags = tags.into_iter().map(CapabilityTag::new).collect::<Vec<_>>();
            exec_config.set_capability_tags(capa_tags);
        }
        exec_config.check_limits()?;

        Ok(exec_config)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn default_runner_fuel_is_finite() {
        assert_eq!(Config::default().runner_fuel(), 1_000_000_000);
    }

    #[test]
    fn default_cell_memory_is_16_mib() {
        assert_eq!(Config::default().cell_memory(), 16 * 1024 * 1024);
    }

    #[test]
    fn max_defaults_to_default() {
        let config = Config::try_from_value(&json!({"runner_fuel": 5, "cell_memory": 65536}))
            .expect("valid config");

        assert_eq!(config.runner_fuel_max(), 5);
        assert_eq!(config.cell_memory_max(), 65536);
    }

    #[test]
    fn explicit_max_is_kept() {
        let config = Config::try_from_value(&json!({
            "runner_fuel": 5,
            "runner_fuel_max": 50,
            "cell_memory": 65536,
            "cell_memory_max": 131_072,
        }))
        .expect("valid config");

        assert_eq!(config.runner_fuel(), 5);
        assert_eq!(config.runner_fuel_max(), 50);
        assert_eq!(config.cell_memory(), 65536);
        assert_eq!(config.cell_memory_max(), 131_072);
    }

    #[test]
    fn default_equal_to_max_is_accepted() {
        let config = Config::try_from_value(&json!({
            "runner_fuel": 5,
            "runner_fuel_max": 5,
            "cell_memory": 65536,
            "cell_memory_max": 65536,
        }))
        .expect("default equal to max is valid");

        assert_eq!(config.runner_fuel(), 5);
        assert_eq!(config.cell_memory(), 65536);
    }

    #[test]
    fn max_fuel_per_handler_is_an_alias() {
        let config =
            Config::try_from_value(&json!({"max_fuel_per_handler": 7})).expect("valid config");

        assert_eq!(config.runner_fuel(), 7);
    }

    #[test]
    fn default_above_max_is_rejected() {
        let cases = [
            (
                json!({"runner_fuel": 10, "runner_fuel_max": 5}),
                "execution.runner_fuel (10) exceeds execution.runner_fuel_max (5)",
            ),
            (
                json!({"cell_memory": 2048, "cell_memory_max": 1024}),
                "execution.cell_memory (2048) exceeds execution.cell_memory_max (1024)",
            ),
        ];

        for (value, expected) in cases {
            let from_value = Config::try_from_value(&value)
                .expect_err("default above max must be rejected")
                .to_string();
            let direct = serde_json::from_value::<Config>(value)
                .expect_err("direct deserialization must reject it too")
                .to_string();

            assert_eq!(from_value, expected);
            assert_eq!(direct, expected);
        }
    }
}
