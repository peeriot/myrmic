# Signal outlets

> **Availability:** embedded runtime only

An outlet is a named output in the Signal Layer. A cell writes it by name, and never names the device behind it.

## Key concepts

The Signal Layer is built from a pipeline file, which declares each outlet's name, the type of its value, and the device it drives. Only one outlet may drive a device.

An outlet the pipeline computes itself cannot be written by a cell, and a cell looking it up finds nothing. Only the outlets the pipeline leaves open are open to a cell.

## When to use

Use an outlet to drive a device through a named interface, without knowing which pin it uses.

Use GPIO to drive the pin directly. GPIO exists on the embedded runtime only.

## Operations

- List the outlets a node offers.
- Look up an outlet by name.
- Write a typed value, or bytes already encoded.

## Example

```rust
use myrmic_sdk::outlet::{list_entry, list_len, Outlet};
use myrmic_sdk::signal_layer::DigitalState;
use myrmic_sdk::{Codec, Postcard};
use myrmic_sdk::Metadata;

#[myrmic_sdk::cmd]
fn set_relay(_md: Metadata, on: bool) -> myrmic_sdk::Result {
    // Every outlet open to this cell. A name comes back as bytes.
    for index in 0..list_len().map_err(|_| "listing outlets failed")? {
        let mut bytes = [0u8; 32];
        let Some((len, _)) = list_entry(index, &mut bytes).map_err(|_| "listing outlets failed")?
        else {
            continue;
        };

        let name = core::str::from_utf8(&bytes[..len]).map_err(|_| "an outlet name was not text")?;

        myrmic_sdk::info!("outlet {name}")?;
    }

    // A name the pipeline does not leave open resolves to nothing, rather than
    // failing.
    let Some(outlet) = Outlet::resolve("relay_cmd").map_err(|_| "outlet lookup failed")? else {
        return Ok(());
    };

    // The value has to match the type the pipeline declared for this outlet.
    // It is encoded into a 64-byte buffer, so a larger value fails here.
    outlet
        .write_typed(&DigitalState { on })
        .map_err(|_| "writing the outlet failed")?;

    // The same command encoded by the cell instead. The bytes go straight to
    // the runtime, so their size is the cell's own business.
    let bytes = Postcard::encode(&DigitalState { on }).map_err(|_| "encoding failed")?;

    outlet
        .write(&bytes)
        .map_err(|_| "writing the outlet failed")?;

    Ok(())
}
```

## Behavior

### Normal

A write replaces the outlet's previous command. What the driver then does is the driver's business: it may clamp the value, or refuse to act on it yet.

A write that succeeds means the Signal Layer took the command. It does not mean the device reached that state. Read a tap for that.

A write takes effect at once. A handler that fails afterwards does not undo it.

### Errors

Looking up or writing an outlet fails when:

- the Signal Layer cannot be reached
- the outlet is no longer available
- the bytes cannot be decoded as the type the pipeline declared

### Limits

Writing a typed value encodes it into a 64-byte buffer, so a larger value fails. Writing bytes hands them over directly, with no limit.

A node holds at most 8 outlets.

A cell that uses an outlet cannot start on a Linux node. It fails while being loaded, before any handler runs.

## API documentation

See the API documentation for [`myrmic_sdk::outlet`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/outlet/index.html), which covers both write operations and the discovery helpers.
