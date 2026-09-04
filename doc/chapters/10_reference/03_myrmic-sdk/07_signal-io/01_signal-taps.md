# Signal taps

> **Availability:** Linux and embedded runtimes

A tap is a named input from the Signal Layer. A cell reads it by name, and never names the sensor or driver the value comes from.

## Key concepts

The Signal Layer is built from a pipeline file, which declares each tap's name, its kind, and the type of its value. A cell looks a tap up and reads it, and declares nothing itself.

A tap is one of two kinds:

- **retained** holds the latest value. Reading it does not consume it, so the same value comes back until a new one arrives.
- **event** holds a queue of values. The cell takes them one at a time, and each value is handed out once.

A tap is never pushed to a cell. The cell reads when it chooses, and a queue that is not read loses its oldest values.

## When to use

Use a tap to read a signal through a named interface, without knowing which pin it comes from. A tap read never blocks the handler.

Use GPIO to read the pin directly, or to wait for it to change. GPIO exists on the embedded runtime only, and waiting for a pin stops the cell and every other call to the runtime on that node until it changes.

## Operations

- Look up a tap by name.
- Read the latest retained value, as a type or as raw bytes.
- Take the next queued value, as a type or as raw bytes.

## Example

```rust
use myrmic_sdk::tap::Tap;
use myrmic_sdk::signal_layer::HealthEvent;
use myrmic_sdk::Metadata;

#[myrmic_sdk::cmd]
fn poll_signals(_md: Metadata) -> myrmic_sdk::Result {
    // A name the pipeline does not offer resolves to nothing, rather than
    // failing.
    let Some(temperature) = Tap::resolve("temperature").map_err(|_| "tap lookup failed")? else {
        return Ok(());
    };

    // The timestamp comes with the value, and nothing comes back before the
    // first value has arrived.
    if let Some((at, value)) = temperature
        .read_typed::<f32>()
        .map_err(|_| "reading the tap failed")?
    {
        myrmic_sdk::info!("{value} at {at}")?;
    }

    let Some(health) = Tap::resolve("_signal_layer_health").map_err(|_| "tap lookup failed")? else {
        return Ok(());
    };

    // An event tap is drained until it is empty. Whatever is not taken now
    // waits for the next time this handler runs.
    while let Some(event) = health
        .take_event_typed::<HealthEvent>()
        .map_err(|_| "taking an event failed")?
    {
        myrmic_sdk::info!("{:?}", event.state)?;
    }

    Ok(())
}
```

## Behavior

### Normal

A retained value carries a timestamp counted in milliseconds by the Signal Layer. It is not the clock a cell reads, and it resets when the Signal Layer restarts.

A retained read gives nothing back before the first value arrives. An event read gives nothing back when the queue is empty.

### Errors

Looking up or reading a tap fails when:

- the Signal Layer cannot be reached
- the tap is no longer available
- a retained value is read from an event tap, or an event is taken from a retained one
- the value cannot be decoded as the type asked for
- the value is larger than 64 bytes, which the embedded runtime refuses and the Linux runtime cuts short

Taking an event removes it before the cell decodes it, so a value that fails to decode is lost. Take raw bytes when that is unacceptable.

### Limits

A value read as a type has to fit in 64 bytes. Read raw bytes for anything larger.

A node holds at most 16 taps.

An event tap holds 8 values. Past that, a new value drops the oldest.

## API documentation

See the API documentation for [`myrmic_sdk::tap`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/tap/index.html), which covers every read operation and the tap kinds.
