//! The structs defining the configuration of the exec plugin live in common, since they also need to be known to
//! the orchestration plugins (they need to know about exec plugin presence and configuration from the info provided
//! by the plugin system).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CapabilityTag, Result, custom_err};

const DEFAULT_INIT_TIMEOUT: Duration = Duration::from_secs(10); // same as zenoh's default
const DEFAULT_FUEL: u64 = u64::MAX;
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
#[serde(from = "ConfigTemplate")]
pub struct Config {
    name: Option<String>,
    runner_fuel: Option<u64>,
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
        Ok(config_template.into())
    }

    pub fn set_capability_tags(&mut self, tags: Vec<CapabilityTag>) -> &mut Self {
        self.tags = tags;
        self
    }

    pub fn set_runner_fuel(&mut self, fuel: u64) -> &mut Self {
        self.runner_fuel = Some(fuel);
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
    runner_fuel: Option<u64>,
    fuel_yield_interval: Option<u64>,
    init_timeout_secs: Option<f64>,
    mailbox_poll_interval_ms: Option<u64>,
    mailbox_batch_size: Option<usize>,
    event_buffer_size: Option<usize>,
    tags: Option<Vec<String>>,
    name: Option<String>,
}

impl From<ConfigTemplate> for Config {
    fn from(config: ConfigTemplate) -> Self {
        let mut exec_config = Config::default();

        if let Some(fuel_yield_interval) = config.fuel_yield_interval {
            exec_config.set_fuel_yield_interval(fuel_yield_interval);
        }
        if let Some(runner_fuel) = config.runner_fuel {
            exec_config.set_runner_fuel(runner_fuel);
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
        exec_config
    }
}
