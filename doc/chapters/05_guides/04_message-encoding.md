# Message Encoding

## Why encoding matters

Commands and events are the messages cells use to communicate. Each may carry a payload, a value exchanged between cells.

Cells run inside a Wasm sandbox, and for a payload to reach its destination it must cross that boundary.

The first problem here is that data can only cross the Wasm sandbox as raw bytes.

The second problem is how to translate those bytes - correctly and consistently on both receiver and sender sides.

This requires an encoding mechanism that is both efficient and flexible.

## What the SDK provides

The Myrmic SDK addresses this through three traits:

- `Encoder` - converts a value to bytes
- `Decoder` - converts bytes back to a value
- `Codec` - defines how values are serialized to bytes

Every payload type must implement `Encoder` and `Decoder`.

The rest of this guide shows how encoding works depending on the payload type, and how to use each one from a handler, the CLI, and another cell.

## Primitives

Primitive types implement `Encoder` and `Decoder` by default.

- `u8`, `u16`, `u32`, `u64`, `u128`, `i8`, `i16`, `i32`, `i64`, `i128`, `f32`, `f64`, `bool`, `char`
- `myrmic_sdk::String`

Their encoded bytes are plain JSON values - `42`, `true`, `"hello"` - so [`myrmic send`](../10_reference/02_myrmic-cli/08_send.md) can pass them directly.

A command handler and an event handler that expect a primitive payload:

```rust
#[myrmic_sdk::cmd]
fn set_threshold(_md: myrmic_sdk::Metadata, value: u32) -> myrmic_sdk::Result {
    Ok(())
}

#[myrmic_sdk::evt]
fn on_reading(_md: myrmic_sdk::Metadata, value: f32) -> myrmic_sdk::Result {
    Ok(())
}
```

Sending from the CLI:

```bash
myrmic send my-cell set_threshold 42
myrmic publish on_reading 22.5
```

Sending from another cell:

```rust
myrmic_sdk::send(target, "set_threshold", &42u32)?;
myrmic_sdk::publish("on_reading", &22.5f32)?;
```

## No payload

When a command or event carries no data, use `Void`, a type that implements `Encoder` and `Decoder` out of the box.

On the handler side, omit the payload argument:

```rust
#[myrmic_sdk::cmd]
fn ping(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    Ok(())
}
```

To send or publish with no payload from other cells:

```rust
myrmic_sdk::send(target, "ping", &myrmic_sdk::Void)?;
myrmic_sdk::publish("heartbeat", &myrmic_sdk::Void)?;
```

From the CLI:

```bash
myrmic send my-cell ping
```

## Custom types

In production, payloads are rarely a single value. Cells exchange structured data - sensor readings, device states, commands with multiple parameters. For these, the payload needs to be described with custom types.

But as mentioned before, for a type to be a payload it must implement `Encoder` and `Decoder` traits.

The SDK solves this with `Message`, a derive macro that generates automatically the implementation of both `Encoder` and `Decoder` for the annotated type.

As mentioned before, `Codec` defines the encoding format. `Message` also gives control over which one to use. Two available encoding formats are:

- `Json` - the default; encodes as human-readable JSON - what the CLI and bridges expect
- `Postcard` - compact binary, suited for constrained devices and performance-sensitive cell-to-cell communication

`Message` relies on [serde](https://docs.rs/serde) for the actual field serialization - custom types must also derive [`Serialize`](https://docs.rs/serde/latest/serde/trait.Serialize.html) and [`Deserialize`](https://docs.rs/serde/latest/serde/trait.Deserialize.html).

Defining a `SensorReading` and choosing a codec:

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Json)] // or myrmic_sdk::Postcard
struct SensorReading {
    sensor_id: myrmic_sdk::String,
    value: f32,
}
```

A command handler and an event handler that expect a `SensorReading`:

```rust
#[myrmic_sdk::cmd]
fn process(_md: myrmic_sdk::Metadata, msg: SensorReading) -> myrmic_sdk::Result {
    Ok(())
}

#[myrmic_sdk::evt]
fn on_reading(_md: myrmic_sdk::Metadata, msg: SensorReading) -> myrmic_sdk::Result {
    Ok(())
}
```

Sending a `SensorReading` from the CLI:

```bash
myrmic send my-cell process '{"sensor_id":"sensor-01","value":22.5}'
```

If the struct uses `Postcard` as `Codec`, pass a hex-encoded Postcard payload with `--raw` instead:

```bash
myrmic send my-cell process --raw <postcard-hex>
```

Sending a `SensorReading` from another cell:

```rust
myrmic_sdk::send(target, "process", &SensorReading {
    sensor_id: "sensor-01".into(),
    value: 22.5,
})?;
```

## Raw bytes

In case the payload is pre-encoded or opaque binary, the SDK provides `Bytes`, which carries the payload as raw bytes with no format applied.

A command handler that receives raw bytes:

```rust
#[myrmic_sdk::cmd]
fn upload(_md: myrmic_sdk::Metadata, data: myrmic_sdk::Bytes) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("received {} bytes", data.len()).ok();
    Ok(())
}
```

From the CLI, pass a hex-encoded payload with `--raw`:

```bash
myrmic send my-cell upload --raw deadbeef
```

Sending image bytes from another cell:

```rust
let image: myrmic_sdk::Bytes = capture_frame();
myrmic_sdk::send(target, "upload", &image)?;
```

## Dynamic JSON

When a payload comes from an external source - such as an MQTT bridge forwarding messages from a broker - its shape is not controlled by the cell. For these cases, the SDK provides `JsonValue`, a type that can hold any valid JSON value: objects, arrays, strings, numbers.

An event handler receiving a temperature reading from an MQTT bridge:

```rust
#[myrmic_sdk::evt]
fn on_temperature(_md: myrmic_sdk::Metadata, measurement: myrmic_sdk::JsonValue) -> myrmic_sdk::Result {
    let celsius = measurement["celsius"].as_f64().unwrap_or(0.0);
    Ok(())
}
```

To simulate a bridge event from the CLI:

```bash
myrmic publish on_temperature '{"celsius":22.5}'
```

Publishing a `JsonValue` from another cell:

```rust
let value: myrmic_sdk::JsonValue = /* ... */;
myrmic_sdk::publish("on_temperature", &value)?;
```

When the payload shape is known, use a typed struct instead.

`JsonValue` is a re-export of [`serde_json::Value`](https://docs.rs/serde_json/latest/serde_json/enum.Value.html).

## See also

- [How to work with commands](./02_commands.md)
- [How to publish and handle events](./03_events.md)
- [CLI Reference](../10_reference/02_myrmic-cli.md)

## Related SDK reference

- [Message encoding](../10_reference/03_myrmic-sdk/02_messaging/04_message-encoding.md)
- [Commands](../10_reference/03_myrmic-sdk/02_messaging/01_commands.md)
- [Events](../10_reference/03_myrmic-sdk/02_messaging/03_events.md)
