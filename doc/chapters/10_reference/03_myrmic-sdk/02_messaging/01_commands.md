# Commands

> **Availability:** Linux and embedded runtimes

A command is a named message sent to one cell to ask it to do something. The command name selects a handler on that cell.

## When to use

Use a command when the sender knows which cell should act. Use a callback alongside it when the sender also needs a reply.

Use an event instead when announcing something that happened, without choosing which cells react.

## Operations

- Declare a handler for a command name.
- Receive a typed payload in that handler, or none.
- Send a command to a cell, with a payload or without one.

## Example

```rust
use myrmic_sdk::{Metadata, Sri};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
struct WorkItem {
    priority: u32,
}

// The handler's own name is the command name unless it is overridden here.
#[myrmic_sdk::cmd(name = "process")]
fn process_item(_md: Metadata, item: WorkItem) -> myrmic_sdk::Result {
    myrmic_sdk::info!("priority={}", item.priority)?;

    Ok(())
}

fn dispatch(worker: Sri) -> myrmic_sdk::Result {
    // Returns once the runtime has accepted the send, not once the handler ran.
    myrmic_sdk::send(worker, "process", &WorkItem { priority: 2 })
}
```

## Behavior

### Normal

Sending does not wait for anyone. A success means the runtime accepted the command, not that the receiving cell ran it.

The payload is decoded before the handler runs. A handler that declares no payload accepts none.

Everything a command handler does commits atomically when it completes, including any command it sent, so nothing is visible outside until then. What happens depends on the outcome:

- **The handler succeeds.** Its work commits and the command is done with.
- **The handler fails or traps.** Nothing it did takes effect, and the command is delivered again.
- **The cell has no handler for that name.** The command is discarded and logged on the node, and the sender is never told.

### Errors

Sending fails when the command name is invalid, when the payload cannot be encoded, when no cell exists at that identity, or when the runtime rejects the request.

A command name may contain only ASCII letters, digits, and underscores, and may not be empty or contain whitespace.

## API documentation

See [`cmd`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/attr.cmd.html) and [`send`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.send.html) in the API documentation.
