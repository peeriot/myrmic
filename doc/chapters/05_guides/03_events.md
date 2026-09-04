# Work with events

## What is an event

An event is a message broadcast from one cell to all interested cells, informing them that something happened. It has a name, a handler, and a payload.

This guide puts events in practice and shows how to use them.

## Event Handler

An event handler is a Rust function annotated with the `#[evt]` macro provided by the Myrmic SDK.

```rust
#[myrmic_sdk::evt]
fn heartbeat(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    // ...
    Ok(())
}
```

`#[evt]` exports the annotated function as a Wasm event handler. During deployment, the runtime discovers all exported event handlers and registers them automatically. By default, the event name is the handler function name - it is the name publishers use to broadcast the event.

To override the event name, pass the `name` attribute to the macro:

```rust
#[myrmic_sdk::evt(name = "heartbeat")]
fn on_heartbeat(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    // ...
    Ok(())
}
```

Every event handler takes `Metadata` as its first argument - it carries context about who published the event and which cell is running:

```rust
pub struct Metadata {
    pub id: Sri,     // the SRI of this cell
    pub sender: Sri, // the SRI of the cell that published the event; nil when published externally (e.g. CLI)
}
```

## Event payload

An event may carry a payload - in that case, add a second argument to the handler to receive it:

```rust
#[myrmic_sdk::evt]
fn temperature_changed(_md: myrmic_sdk::Metadata, value: f32) -> myrmic_sdk::Result {
    // ...
    Ok(())
}
```

The payload type must implement `Decoder` and `Encoder` - traits the SDK uses to deserialize and serialize the payload. Both rely on [`serde::Serialize`](https://docs.rs/serde/latest/serde/trait.Serialize.html) and [`serde::Deserialize`](https://docs.rs/serde/latest/serde/trait.Deserialize.html), which must also be derived. To understand why and how, see [Message encoding](./04_message-encoding.md).

Numeric primitives (`f32`, `u32`, `i32`, `bool` ...), and types provided by the SDK such as `myrmic_sdk::String`, `myrmic_sdk::Bytes`, and `myrmic_sdk::JsonValue` already implement it by default.

An event handler is limited to one payload argument - if you need multiple fields values, wrap them in a struct and derive `myrmic_sdk::Message` to automatically implement `Encoder` and `Decoder`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
struct TemperatureChanged {
    sensor_id: myrmic_sdk::String,
    value: f32,
}

#[myrmic_sdk::evt]
fn temperature_changed(_md: myrmic_sdk::Metadata, event: TemperatureChanged) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("sensor {} reported {:.1}°C", event.sensor_id, event.value).ok();
    Ok(())
}
```

## Publish an event from the CLI

Rather than writing a cell just to test, the CLI provides [`myrmic publish`](../10_reference/02_myrmic-cli/09_publish.md) - a tool that broadcasts an event directly from the terminal.

It takes an event name and an optional payload, and broadcasts it to all cells with a matching handler:

```bash
# No payload
myrmic publish heartbeat

# JSON payload
myrmic publish temperature_changed '{"sensor_id":"sensor-01","value":22.5}'
```

The payload is sent as JSON by default. For a full reference see [`myrmic publish`](../10_reference/02_myrmic-cli/09_publish.md).

## Publish an event from a cell

Now we see how a cell publishes an event from within its own handlers.

```rust
#[myrmic_sdk::cmd]
fn record(_md: myrmic_sdk::Metadata, value: f32) -> myrmic_sdk::Result {
    let event = TemperatureChanged {
        sensor_id: myrmic_sdk::String::from("sensor-01"),
        value,
    };

    myrmic_sdk::publish("temperature_changed", &event)?;

    Ok(())
}
```

The Myrmic SDK provides the `publish` function to broadcast events. It takes the event name and the payload.

`publish` is callable from any handler within a cell - a command handler, an event handler, or an init initialization handler.

For events with no payload, the SDK provides `myrmic_sdk::Void` - a zero-sized placeholder that signals no data is being sent:

```rust
#[myrmic_sdk::cmd]
fn heartbeat(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    myrmic_sdk::publish("heartbeat", &myrmic_sdk::Void)?;
    Ok(())
}
```

## Dispatch from an event handler

`publish` is non-blocking - execution continues immediately after each call. All event publishes (and command sends) made inside an event handler commit atomically together with any storage operations when the handler completes - nothing is visible to the outside until that point.

## See also

- [How to work with commands](./02_commands.md)
- [Scheduling Handlers](./05_scheduling-handlers.md) - how to schedule work at a fixed interval or after a delay
- [Message encoding](./04_message-encoding.md)
- [State and Storage](./06_state-and-storage.md) - working with state and storage
- [`myrmic publish` reference](../10_reference/02_myrmic-cli/09_publish.md)

## Related SDK reference

- [Events](../10_reference/03_myrmic-sdk/02_messaging/03_events.md)
- [Message encoding](../10_reference/03_myrmic-sdk/02_messaging/04_message-encoding.md)
