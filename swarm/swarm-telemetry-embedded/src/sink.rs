use embassy_sync::{blocking_mutex::raw::RawMutex, channel};

use crate::logger::Sink;
use crate::record::TelemetryRecord;

pub struct ChannelSink<M: RawMutex + 'static, const N: usize> {
    channel: &'static channel::Channel<M, TelemetryRecord, N>,
}

impl<M: RawMutex + 'static, const N: usize> ChannelSink<M, N> {
    pub const fn new(channel: &'static channel::Channel<M, TelemetryRecord, N>) -> Self {
        Self { channel }
    }
}

impl<M: RawMutex + Send + Sync + 'static, const N: usize> Sink for ChannelSink<M, N> {
    fn send(&self, record: TelemetryRecord) {
        let _ = self.channel.try_send(record);
    }

    fn flush(&self) {}
}
