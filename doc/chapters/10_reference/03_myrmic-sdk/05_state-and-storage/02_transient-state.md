# Transient state

> **Availability:** Linux and embedded runtimes

Transient state holds a value in the cell's WebAssembly memory rather than the runtime database, so it needs no encoding.

## When to use

Use transient state for a value that is cheap to rebuild and does not need to outlive the cell.

Use persistent state or a store when the value has to survive the cell restarting.

## Operations

- Declare a value in the cell's memory.
- Read or change it inside a closure.
- Borrow it instead, for as long as the caller needs.

## Example

```rust
use myrmic_sdk::ble::ScanHandle;
use myrmic_sdk::{InMemory, Metadata};

// A scan handle means nothing to another cell and cannot be stored.
static SCAN: InMemory<Option<ScanHandle>> = InMemory::empty();

fn keep(handle: ScanHandle) -> myrmic_sdk::Result {
    // Replaces whatever was there. Later invocations see this value.
    SCAN.with(|slot| *slot = Some(handle))
}

#[myrmic_sdk::cmd]
fn stop_scan(_md: Metadata) -> myrmic_sdk::Result {
    // Held for the rest of the function, so the handle can be taken out and
    // used while the slot stays borrowed.
    let mut slot = SCAN.try_borrow_mut()?;

    if let Some(handle) = slot.take() {
        handle.stop()?;
    }

    Ok(())
}
```

## Behavior

### Normal

A value set by one of the cell's handlers - initialization, command, event, or monitor - is still there for the next, for as long as the cell runs.

### Errors

A borrow taken while the value is already borrowed fails with an error rather than panicking.

### Limits

The value is gone whenever the cell starts again.

## API documentation

For every method on the handle, see [`InMemory`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/struct.InMemory.html).
