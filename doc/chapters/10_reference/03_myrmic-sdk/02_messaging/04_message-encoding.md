# Message encoding

> **Availability:** Linux and embedded runtimes

Message encoding turns a value into bytes for sending, and turns received bytes back into a value.

Both sides must use the same format, and the same shape of data. With JSON the field names travel with the value; with the binary format they do not, so a different type of the same shape decodes without complaint into the wrong value.

## Operations

- Declare a type as a message, so it can be sent and received.
- Choose the encoding format a message uses.
- Encode or decode a value yourself.
- Send a payload whose shape is not fixed.
- Send bytes with no encoding applied.
- Send no payload at all.

## Example

```rust
use myrmic_sdk::{Bytes, Metadata};
use serde::{Deserialize, Serialize};

// JSON is the default, so this attribute could be left out.
#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Json)]
struct Reading {
    sensor: myrmic_sdk::String,
    value: f64,
}

// The binary format is worth choosing where payload size matters.
#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct CompactReading {
    value: i16,
}

// A handler names the type it expects, and receives it already decoded.
#[myrmic_sdk::cmd]
fn on_reading(_md: Metadata, reading: Reading) -> myrmic_sdk::Result {
    myrmic_sdk::info!("{} = {}", reading.sensor, reading.value)?;

    Ok(())
}

// A handler taking bytes receives the payload exactly as it was sent.
#[myrmic_sdk::cmd]
fn on_image(_md: Metadata, image: Bytes) -> myrmic_sdk::Result {
    myrmic_sdk::info!("{} bytes", image.len())?;

    Ok(())
}

// A handler with no payload parameter accepts an empty payload only.
#[myrmic_sdk::cmd]
fn on_ping(_md: Metadata) -> myrmic_sdk::Result {
    Ok(())
}
```

## Behavior

### Normal

Declaring a type as a message gives it both encoding and decoding, using JSON unless another format is chosen.

Sending and receiving encode and decode for you, so most code never does either itself.

Bytes pass through unchanged, which suits content the application already has in its final form, such as an image.

A handler that takes no payload accepts an empty one only. Sending it a payload is rejected rather than quietly ignored.

### Errors

Encoding fails when the value cannot be represented in the chosen format.

Decoding fails when the bytes were produced by a different format, or do not match the type the handler declares.

### Limits

The type's name is not sent, so a mismatch between the two sides is not detected for you. Adding, removing, or renaming a field can leave existing senders and receivers unable to read each other.

## API documentation

For the derive's options and the traits behind it, see [`Message`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/derive.Message.html), [`Codec`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/trait.Codec.html), [`Encoder`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/trait.Encoder.html) and [`Decoder`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/trait.Decoder.html).
