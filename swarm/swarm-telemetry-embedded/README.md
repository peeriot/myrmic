# swarm-telemetry-embedded

Embedded-side telemetry framework for Swarm devices. Collects log records via
the standard [`log`] facade and publishes them as JSON over zenoh so the edge
can receive and forward them into the normal OpenTelemetry pipeline.

## Wiring

### 1. Initialise with the builder

```rust
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // session is your ZNSession<'static, NoopRawMutex>
    TelemetryBuilder::new()
        .level(log::LevelFilter::Info)
        .init(spawner, session);
}
```

The logger is registered, the log level is set, and the telemetry task is spawned.
Records are silently dropped when the channel is full (default capacity: 8).

For a custom channel capacity use [`ChannelSink`] and [`TelemetryLogger`]
directly.

