# GPIO

> **Availability:** embedded runtime only

GPIO gives a cell direct access to numbered digital pins on an embedded node. A pin can be read, driven high or low, or waited on for a level or an edge.

## Operations

- Ask for a numbered pin.
- Read whether a pin is high or low.
- Drive a pin high or low.
- Wait until a pin is high or low, or until it changes.

## Example

The cell's crate needs `embedded-hal` as a dependency, for the read and write traits.

```rust
use embedded_hal::digital::{InputPin, OutputPin};
use myrmic_sdk::gpio::{Gpio4, Gpio5, Wait};
use myrmic_sdk::{Metadata, Result};

#[myrmic_sdk::cmd]
fn mirror_button(_md: Metadata) -> Result<()> {
    // None for a pin this node does not offer.
    let Some(mut button) = Gpio4::try_get() else {
        return Err("GPIO 4 is unavailable");
    };
    let Some(mut led) = Gpio5::try_get() else {
        return Err("GPIO 5 is unavailable");
    };

    // Read the button and drive the indicator to the level just read.
    if button.is_high().map_err(|_| "cannot read GPIO 4")? {
        led.set_high().map_err(|_| "cannot drive GPIO 5")?;
    } else {
        led.set_low().map_err(|_| "cannot drive GPIO 5")?;
    }

    // Nothing else runs on this node until the button changes level.
    button.wait_for_any_edge().map_err(|_| "cannot wait on GPIO 4")?;

    Ok(())
}
```

## Behavior

### Normal

Asking for a pin only checks that the node offers it. The pin is never reserved, so it can be used from anywhere in the cell.

A read reports the level at that moment. A write returns once the node has written the pin.

Waiting for a level returns at once if the pin is already at it. Waiting for a change always waits. For example, a pin that is already high does not satisfy a wait for a rise: it has to go low and high again.

### Errors

A read, a write, or a wait fails when the node does not offer the pin. Nothing tells the cell any more than that.

### Limits

Waiting has no timeout and cannot be cancelled. Until it returns, the cell does nothing else, and the node cannot serve any other call - including deploying or deleting a cell.

A node offers only the pins its chip has and nothing else already uses.

Electrical limits, pull resistors, drive strength, and safe pin use depend on the board and lie outside this API.

## API documentation

See the API documentation for [`myrmic_sdk::gpio`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/gpio/index.html), which covers the pin types and the blocking wait trait.
