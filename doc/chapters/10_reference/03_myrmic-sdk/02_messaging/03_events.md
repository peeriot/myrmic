# Events

> **Availability:** Linux and embedded runtimes

An event is a named message that any number of cells can receive. The publisher does not choose who reacts to it.

## When to use

Use an event when the publisher should not know or choose the recipients.

Use a command when one specific cell should perform an operation.

## Operations

- Declare a handler for an event name.
- Receive a typed payload in that handler, or none.
- Publish an event, with a payload or without one.

## Example

```rust
use myrmic_sdk::Metadata;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
struct TemperatureChanged {
    value: f32,
}

// Declaring this handler is what subscribes the cell to the event.
#[myrmic_sdk::evt(name = "temperature_changed")]
fn on_temperature(_md: Metadata, event: TemperatureChanged) -> myrmic_sdk::Result {
    myrmic_sdk::info!("temperature={}", event.value)?;

    Ok(())
}

fn announce(value: f32) -> myrmic_sdk::Result {
    myrmic_sdk::publish("temperature_changed", &TemperatureChanged { value })
}
```

## Behavior

### Normal

Declaring a handler subscribes the cell to that event name. There is no subscription call, and a cell cannot subscribe to an event it has no handler for.

A cell only receives events published after it was deployed. An event published while no cell is subscribed is not delivered later.

Publishing does not wait for anyone. A success means the runtime accepted the event, not that a cell received or handled it.

The payload is decoded before the handler runs. A handler also receives the publisher's identity, which is empty when the event came from outside a cell.

Everything an event handler does commits atomically when it completes, including any event it published, so nothing is visible outside until then. What happens depends on the outcome:

- **The handler succeeds.** Its work commits.
- **The handler fails or traps.** Nothing it did takes effect, but the event is not delivered again.

### Errors

Publishing fails when the event name is invalid, when the payload cannot be encoded, or when the runtime rejects it. An event name may contain only ASCII letters, digits, and underscores, and may not be empty or contain whitespace.

An event whose payload does not match the type a handler declares is skipped and logged on the node. The handler never runs for it.

## API documentation

See [`evt`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/attr.evt.html) and [`publish`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.publish.html) in the API documentation.
