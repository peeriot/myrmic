# Cell termination

> **Availability:** Linux runtime only

A cell can remove itself or one of its descendants. It can also stop itself and report an optional exit code to its parent.

## When to use

Use termination when a cell must remove itself or a descendant.

Use self-stop when the cell has reached a deliberate final state of its own.

## Operations

- Terminate a cell, identified by either its UUID or its SRN path.
- Stop the current cell.
- Stop the current cell and report an exit code to its parent.

## Example

```rust
use myrmic_sdk::Metadata;

#[myrmic_sdk::cmd]
fn shut_down(_md: Metadata) -> myrmic_sdk::Result {
    // The target may be written as a UUID or as an SRN path.
    myrmic_sdk::terminate_cell("myapp/gateway/worker-1")?;

    // Reports the code to this cell's parent. Passing nothing reports no code.
    myrmic_sdk::stop_self(Some(0));

    // Nothing after this line is guaranteed to run.
    Ok(())
}
```

## Behavior

### Normal

Terminating a cell also removes its descendants, except any that were spawned as independent. A cell may only terminate itself or one of its own descendants.

Stopping the current cell does the same to its descendants. When the cell has a parent, that parent is told the child stopped, along with the exit code if one was given. Only the named cell's loss is reported, never the descendants removed with it.

Removing a cell does not delete what it stored.

Termination takes effect immediately. If the handler that asked for it then fails, its writes and messages are rolled back, but the cell is gone.

### Errors

Termination fails when:

- the target cannot be found, whether the name is malformed or no such cell exists
- the target is not the caller or one of its descendants
- removing its deployment or its registration fails

### Limits

A cell that stops itself is not removed on the spot. The call returns at once and the removal follows on its own, so the cell can be cut off at any point after it and code following the call may never run. Complete any state changes and outgoing messages before stopping, and never rely on that code for cleanup or correctness.

## API documentation

For exact signatures and every error, see [`terminate_cell`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.terminate_cell.html), [`stop_self`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.stop_self.html) and [`TerminateError`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/enum.TerminateError.html).
