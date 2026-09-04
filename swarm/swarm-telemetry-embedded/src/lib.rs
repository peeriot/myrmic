#![no_std]

pub mod logger;
mod record;
pub mod sink;
pub mod task;

pub use logger::{Sink, TelemetryLogger};
pub use record::{Level, TelemetryRecord};
pub use sink::ChannelSink;

pub const TOPIC_LOGS: &str = "@telemetry/@v1/@embedded/@logs";

/// the default logger has a channel with this size
static DEFAULT_CHANNEL_SIZE: usize = 8;
static DEFAULT_CHANNEL: embassy_sync::channel::Channel<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    TelemetryRecord,
    DEFAULT_CHANNEL_SIZE,
> = embassy_sync::channel::Channel::new();
static DEFAULT_LOGGER: TelemetryLogger<
    ChannelSink<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, DEFAULT_CHANNEL_SIZE>,
> = TelemetryLogger::new(ChannelSink::new(&DEFAULT_CHANNEL), log::LevelFilter::Trace);

/// Builder for telemetry initialisation.
///
/// ```ignore
/// TelemetryBuilder::new()
///     .level(log::LevelFilter::Info)
///     .init(spawner, session);
/// ```
pub struct TelemetryBuilder {
    max_level: log::LevelFilter,
}

impl Default for TelemetryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryBuilder {
    pub fn new() -> Self {
        Self {
            max_level: log::LevelFilter::Info,
        }
    }

    pub fn level(mut self, level: log::LevelFilter) -> Self {
        self.max_level = level;
        self
    }

    pub fn init(
        self,
        spawner: embassy_executor::Spawner,
        session: zenoh_traits::nano::ZNSession<
            'static,
            embassy_sync::blocking_mutex::raw::NoopRawMutex,
        >,
    ) {
        log::set_logger(&DEFAULT_LOGGER).expect("logger already set");
        log::set_max_level(self.max_level);
        spawner.spawn(task::telemetry_task(session, DEFAULT_CHANNEL.receiver().into()).unwrap());
    }
}
