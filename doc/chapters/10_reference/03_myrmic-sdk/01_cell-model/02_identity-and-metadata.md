# Cell identity and metadata

> **Availability:** Linux and embedded runtimes

A cell's identity has two forms: an SRN, the readable name it is deployed under, such as `myapp/fleet`, and an SRI, a UUID that is derived from the SRN. Neither is a network address or a port.

The runtime uses the SRI internally. The SRN is human readable, so it is the form users work with when interacting with cells, through the CLI or in the app manifest file.

Every handler (command, event or initialization) takes metadata as its first argument. It is what the runtime says about the invocation: the SRI of the cell being invoked, and the SRI of whoever triggered it.

## Operations

- Read the SRI of the running cell from the invocation metadata.
- Read the SRI of the sender from the same metadata.
- Turn a string that names a cell, whether a UUID or an SRN path, into an SRI.
- Derive an SRI from an SRN path.
- Derive the SRI a child will get from the name it will be spawned under.
- Convert an SRI to a UUID and back.

## Example

```rust
use myrmic_sdk::{Metadata, Sri, String};

#[myrmic_sdk::cmd]
fn inspect(md: Metadata, cell: String) -> myrmic_sdk::Result {
    // A nil sender means no cell triggered this invocation.
    if md.sender.is_nil() {
        myrmic_sdk::info!("{} was invoked externally", md.id)?;
    }

    // The payload came from outside, so it may be a UUID or an SRN path.
    let named = Sri::from_target(&cell)
        .map_err(|_| "neither a UUID nor a valid SRN path")?;

    // This SRN is written in the code, so deriving it is enough.
    let fleet = Sri::of_path("myapp/fleet")
        .map_err(|_| "invalid SRN path")?;

    // The SRI this child will get once it is spawned.
    let sensor = fleet.child("sensor-1")
        .map_err(|_| "invalid child name")?;

    // An SRI is a UUID underneath, and converts both ways.
    let sensor = Sri::from(sensor.as_uuid());

    myrmic_sdk::info!("target {named}, sensor {sensor}")?;

    Ok(())
}
```

## Behavior

### Normal

At initialization the sender is the cell that spawned this one, and is nil only for a root cell. Afterwards it is nil for a command from the CLI, the session's SRI for a command through the gateway, so the cell can reply to it, and the cell's own SRI for a scheduled invocation.

A string that parses as a UUID is taken as an SRI unchanged. Anything else is derived as an SRN path.

The cell derives an SRI itself internally, without including the runtime.

Deriving the same SRN path always produces the same SRI.

### Errors

Deriving fails when a segment of the SRN is not a valid name, such as an empty segment or one with a disallowed character.

### Limits

An SRI names a cell, not a place. Holding one does not prove that the cell exists, is running, or can be reached.

An SRI cannot be turned back into its SRN.

Metadata carries those two SRIs and nothing else: not the cell's SRN, not the name of the message being handled, nothing about the payload.

## API documentation

For every constructor and conversion, see [`Metadata`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/struct.Metadata.html) and [`Sri`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/struct.Sri.html).
