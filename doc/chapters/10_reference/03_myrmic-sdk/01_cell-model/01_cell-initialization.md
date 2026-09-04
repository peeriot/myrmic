# Cell initialization

> **Availability:** Linux and embedded runtimes

An initialization handler prepares a cell before it starts handling messages. The runtime calls it once, and the cell handles nothing until it returns.

## When to use

Use an initialization handler when a cell must read data passed at deployment, create initial persistent state, register routes, or schedule work before it handles messages.

Omit it when the cell has no setup work. Use a command, event, or monitor handler for work that happens after initialization.

## Operations

- Declare one function as the cell's initialization handler.
- Receive data passed at deployment as a typed value, or none at all.

## Example

```rust
use myrmic_sdk::Metadata;
use serde::{Deserialize, Serialize};

// The type is decoded from data passed at deployment
// serializable in both directions.
#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
struct Config {
    sample_period_ms: u64,
}

#[myrmic_sdk::init]
fn init(md: Metadata, config: Config) -> myrmic_sdk::Result {
    // The metadata carries this cell's own identity.
    myrmic_sdk::info!("starting {} with period {}", md.id, config.sample_period_ms)?;

    Ok(())
}
```

## Behavior

### Normal

The runtime calls the handler once, when the cell starts. It supplies the cell's own identity, the data passed at deployment, and for a spawned cell the identity of the cell that spawned it. A root cell has no spawning cell, so that identity is empty.

The data is decoded before the handler runs. A handler that declares no payload accepts none.

The handler is expected to finish its setup and return. It is not a long-running process.

### Errors

Initialization fails when the data passed at deployment does not match the declared type, when a handler that declares no payload receives data anyway, or when the handler itself returns an error.

## API documentation

See [`init`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/attr.init.html) in the API documentation.
