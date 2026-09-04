# Cell monitoring

> **Availability:** Linux runtime only

A monitor handler tells a parent that one of its supervised children was lost. The notification names the child and explains why.

## When to use

Use a monitor handler when a parent must react to a child ending, whether that was deliberate or not.

Use commands, callbacks, or events for ordinary application data between cells.

## Operations

- Declare the cell's monitor handler.
- Read which child was lost, and the local name it was given when spawned.
- Distinguish why it was lost: it stopped, it crashed, it was terminated, its node was lost, or it never started.
- Read the exit code a child reported when it stopped itself.

## Example

```rust
use myrmic_sdk::Metadata;
use myrmic_sdk::monitor::{CellLost, LostReason};

#[myrmic_sdk::monitor]
fn on_cell_lost(_md: Metadata, loss: CellLost) -> myrmic_sdk::Result {
    // The name given at spawn, absent when the runtime named the child. Its
    // identity is always in loss.cell.
    let child = loss.local_name.as_deref().unwrap_or("unnamed");

    match loss.reason {
        // Stopped itself, so it reached a final state of its own.
        LostReason::Stopped { code } => {
            myrmic_sdk::info!("child {} finished: {:?}", child, code)?;
        }
        // Ended on its own without stopping, so it may be worth replacing.
        LostReason::Crashed => {
            myrmic_sdk::warn!("child {} crashed", child)?;
        }
        // Removed by this cell or an ancestor, so no replacement is wanted.
        LostReason::Terminated => {
            myrmic_sdk::info!("child {} was terminated", child)?;
        }
        // Its node went silent, so the work has to move elsewhere.
        LostReason::NodeLost => {
            myrmic_sdk::warn!("child {} lost with its node", child)?;
        }
        // Never ran, so spawning again needs the cause fixed first.
        LostReason::SpawnFailed => {
            myrmic_sdk::error!("child {} never started: {}", child, loss.cell)?;
        }
    }

    Ok(())
}
```

## Behavior

### Normal

A loss notification arrives through the parent's mailbox, so it survives the parent's node restarting. It is consumed when the handler succeeds, and stays queued for another delivery when the handler fails.

The same loss may be reported more than once, so recovery must tolerate a repeat.

A notification can arrive before the child has finished being removed, and a child lost with its node is only presumed gone.

A child spawned as independent never reports its loss to its former parent, and a root cell has no parent to report to. Only the child itself is reported, never the descendants removed with it.

### Limits

A cell can declare only one monitor handler, because the runtime looks for a single entry point by name. A cell that declares none has its notifications dropped.

The notification carries no sender; the lost cell is named in the payload.

## API documentation

See the API documentation for [`myrmic_sdk::monitor`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/monitor/index.html), which covers the notification type and every loss reason.
