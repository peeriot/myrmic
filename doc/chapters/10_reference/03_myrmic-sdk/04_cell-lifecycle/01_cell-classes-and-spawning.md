# Cell classes and spawning

> **Availability:** Linux runtime only

A cell class is the deployable definition of a cell: its WebAssembly binary. A running cell can spawn a child from any class registered in the swarm, and each child is a separate instance with its own identity and state.

## When to use

Spawn a cell when you only know at runtime that you need it, such as one cell per device a scan finds.

Deploy a cell up front when it is always there.

## Operations

- Refer to a class.
- Spawn a child from that class.
- Name the child, or let the runtime name it.
- Pass the child an initialization payload.
- Require the child to run on a node carrying particular tags.
- Set how long the child survives silence from its parent.
- Set how long its node may be silent before the child is declared lost.
- Spawn the child so it outlives its parent.

## Example

```rust
use core::time::Duration;
use myrmic_sdk::Metadata;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
struct WorkerConfig {
    device: myrmic_sdk::String,
}

// The deployment resolves this name to the class, so the parent never embeds it.
const WORKER: myrmic_sdk::ClassHandle = myrmic_sdk::declare!("worker");

#[myrmic_sdk::cmd]
fn add_worker(_md: Metadata) -> myrmic_sdk::Result {
    let worker = WORKER
        .new()
        // Optional. Naming the child gives it the same identity every time.
        .name("worker-1")
        // Optional. Decoded by the child's initialization handler.
        .payload(&WorkerConfig { device: "sensor-1".into() })
        // Optional. Only a node carrying this tag may run it.
        .tag("accelerator")
        // Optional. If this parent goes quiet, the child is killed after this
        // long. A longer time lets the child survive a parent restart.
        .grace(Duration::from_secs(30))
        // Optional. If the child's node goes quiet for this long, the child is
        // declared dead and this parent is told.
        .deadline(Duration::from_secs(60))
        // Optional. The child then outlives this parent, neither is told when
        // the other is lost, and the grace above no longer applies.
        // .detached()
        .spawn()?;

    myrmic_sdk::info!("spawned {worker}")?;

    Ok(())
}
```

## Behavior

### Normal

A child's identity comes from its parent and the name it was given, so the same name always means the same child. Spawning that name again fails rather than creating a second one. Letting the runtime name the child produces a new one each time.

A child stops when its parent stops, and its loss is reported to that parent. A detached child does neither, and the grace setting does not apply to it.

An initialization payload must match what the child's initialization handler expects, and is encoded like any other message.

A spawn takes effect immediately. If the handler that spawned the child then fails, its writes and messages are rolled back, but the child stays.

### Errors

Spawning fails when:

- the class cannot be resolved
- the child's identity is already in use
- no available node carries the required tags
- encoding or deployment fails

## API documentation

For every builder option and spawn error, see [`declare`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/macro.declare.html), [`ClassHandle`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/struct.ClassHandle.html), [`SpawnBuilder`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/struct.SpawnBuilder.html) and [`SpawnError`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/enum.SpawnError.html).
