use crate::record::{Level, TelemetryRecord};

struct TruncatingWriter<'a, const N: usize>(&'a mut heapless::String<N>);

impl<const N: usize> core::fmt::Write for TruncatingWriter<'_, N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.chars() {
            if self.0.push(c).is_err() {
                break;
            }
        }
        Ok(())
    }
}

pub trait Sink: Send + Sync {
    fn send(&self, record: TelemetryRecord);
    fn flush(&self);
}

pub struct TelemetryLogger<S: Sink> {
    sink: S,
    max_level: log::LevelFilter,
}

impl<S: Sink> TelemetryLogger<S> {
    pub const fn new(sink: S, max_level: log::LevelFilter) -> Self {
        Self { sink, max_level }
    }
}

impl<S: Sink> log::Log for TelemetryLogger<S> {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.max_level
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let mut message = heapless::String::new();
        let mut target = heapless::String::new();

        let _ = core::fmt::write(&mut TruncatingWriter(&mut message), *record.args());
        let _ = core::fmt::write(
            &mut TruncatingWriter(&mut target),
            format_args!("{}", record.target()),
        );

        self.sink.send(TelemetryRecord {
            level: Level::from(record.level()),
            target,
            message,
        });
    }

    fn flush(&self) {
        self.sink.flush();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::sync::Mutex;
    use std::vec::Vec;

    use super::*;
    use crate::record::Level;

    struct CaptureSink(Mutex<Vec<TelemetryRecord>>);

    impl CaptureSink {
        const fn new() -> Self {
            Self(Mutex::new(Vec::new()))
        }

        fn records(&self) -> std::sync::MutexGuard<'_, Vec<TelemetryRecord>> {
            self.0.lock().unwrap()
        }
    }

    impl Sink for &'static CaptureSink {
        fn send(&self, record: TelemetryRecord) {
            self.0.lock().unwrap().push(record);
        }
    }

    #[test]
    fn logger_converts_record_fields() {
        static SINK: CaptureSink = CaptureSink::new();
        let logger = TelemetryLogger::new(&SINK, log::LevelFilter::Trace);

        log::Log::log(
            &logger,
            &log::Record::builder()
                .level(log::Level::Warn)
                .target("my_crate::module")
                .args(format_args!("something went wrong"))
                .build(),
        );

        let records = SINK.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level, Level::Warn);
        assert_eq!(records[0].target.as_str(), "my_crate::module");
        assert_eq!(records[0].message.as_str(), "something went wrong");
    }

    #[test]
    fn logger_truncates_long_message() {
        static SINK: CaptureSink = CaptureSink::new();
        let logger = TelemetryLogger::new(&SINK, log::LevelFilter::Trace);
        let long_message = "x".repeat(300);

        log::Log::log(
            &logger,
            &log::Record::builder()
                .level(log::Level::Info)
                .target("t")
                .args(format_args!("{}", long_message))
                .build(),
        );

        let records = SINK.records();
        assert_eq!(records[0].message.len(), 256);
    }

    #[test]
    fn logger_truncates_long_target() {
        static SINK: CaptureSink = CaptureSink::new();
        let logger = TelemetryLogger::new(&SINK, log::LevelFilter::Trace);
        let long_target = "x".repeat(100);

        log::Log::log(
            &logger,
            &log::Record::builder()
                .level(log::Level::Info)
                .target(&long_target)
                .args(format_args!("msg"))
                .build(),
        );

        let records = SINK.records();
        assert_eq!(records[0].target.len(), 64);
    }

    #[test]
    fn logger_filters_below_max_level() {
        static SINK: CaptureSink = CaptureSink::new();
        let logger = TelemetryLogger::new(&SINK, log::LevelFilter::Warn);

        log::Log::log(
            &logger,
            &log::Record::builder()
                .level(log::Level::Debug)
                .target("t")
                .args(format_args!("verbose"))
                .build(),
        );

        assert!(SINK.records().is_empty());
    }
}
