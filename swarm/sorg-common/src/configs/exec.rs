//! The structs defining the configuration of the exec plugin live in common, since they also need to be known to
//! the orchestration plugins (they need to know about exec plugin presence and configuration from the info provided
//! by the plugin system).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::CapabilityTag;

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

/// The exec plugin's configuration, as the `execution` section of a runtime's config file has
/// it: a field the file leaves out takes its default. The durations are kept in the file's units
/// (seconds, milliseconds), which their getters and setters convert.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    name: Option<String>,
    runner_fuel: Option<u64>,
    fuel_yield_interval: Option<u64>,
    init_timeout_secs: Option<f64>,
    mailbox_poll_interval_ms: Option<u64>,
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
            init_timeout_secs: None,
            mailbox_poll_interval_ms: Some(millis(DEFAULT_MAILBOX_POLL_INTERVAL)),
            mailbox_batch_size: Some(DEFAULT_MAILBOX_BATCH_SIZE),
            event_buffer_size: Some(DEFAULT_EVENT_BUFFER_SIZE),
            tags: vec![],
        }
    }
}

impl Config {
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
        self.init_timeout_secs = Some(init_timeout.as_secs_f64());
        self
    }

    pub fn set_mailbox_poll_interval(&mut self, interval: Duration) -> &mut Self {
        self.mailbox_poll_interval_ms = Some(millis(interval));
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
        self.init_timeout_secs
            .map_or(DEFAULT_INIT_TIMEOUT, Duration::from_secs_f64)
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
        self.mailbox_poll_interval_ms
            .map_or(DEFAULT_MAILBOX_POLL_INTERVAL, Duration::from_millis)
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

/// `duration` in whole milliseconds, as the config file has it
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).expect("a config duration's milliseconds fit a u64")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Config, DEFAULT_EVENT_BUFFER_SIZE};
    use crate::CapabilityTag;

    /// A config file's `execution` section reads into its settings, durations in the file's units,
    /// and what it leaves out takes the default.
    #[test]
    fn a_config_file_section_reads_into_its_settings() -> serde_json::Result<()> {
        let section = serde_json::json!({
            "init_timeout_secs": 1.5,
            "mailbox_poll_interval_ms": 250,
            "mailbox_batch_size": 32,
            "tags": ["gpu"],
        });

        let config: Config = serde_json::from_value(section)?;

        assert_eq!(config.init_timeout(), Duration::from_millis(1500));
        assert_eq!(config.mailbox_poll_interval(), Duration::from_millis(250));
        assert_eq!(config.mailbox_batch_size(), 32);
        assert_eq!(config.capability_tags(), [CapabilityTag::new("gpu")]);
        assert_eq!(config.event_buffer_size(), DEFAULT_EVENT_BUFFER_SIZE);
        Ok(())
    }

    /// A config written out reads back the same: writing and reading use one format.
    #[test]
    fn a_config_reads_back_what_it_writes() -> serde_json::Result<()> {
        let mut config = Config::default();
        config
            .set_init_timeout(Duration::from_millis(1500))
            .set_mailbox_poll_interval(Duration::from_millis(250))
            .set_mailbox_batch_size(32)
            .set_event_buffer_size(64)
            .set_capability_tags(vec![CapabilityTag::new("gpu")])
            .set_name("exec".to_owned());

        let read: Config = serde_json::from_value(serde_json::to_value(&config)?)?;

        assert_eq!(read.init_timeout(), Duration::from_millis(1500));
        assert_eq!(read.mailbox_poll_interval(), Duration::from_millis(250));
        assert_eq!(read.mailbox_batch_size(), 32);
        assert_eq!(read.event_buffer_size(), 64);
        assert_eq!(read.capability_tags(), [CapabilityTag::new("gpu")]);
        assert_eq!(read.name(), Some("exec"));
        assert_eq!(read.runner_fuel(), config.runner_fuel());
        assert_eq!(read.fuel_yield_interval(), config.fuel_yield_interval());
        Ok(())
    }
}
