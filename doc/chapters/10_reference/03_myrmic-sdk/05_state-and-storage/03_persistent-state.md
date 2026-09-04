# Persistent cell state

> **Availability:** Linux and embedded runtimes

Persistent state keeps one typed value under one key in the runtime database. The value stays available from one handler to the next, and across restarts of the cell.

## When to use

Use persistent state for a single value that has to survive the cell restarting, such as a counter or a setting.

Use a key-value store or a table when the data holds many entries. Use transient state for values that should not survive a restart.

## Operations

- Declare a handle for a key, fixed at compile time or built while running.
- Declare which scope the value belongs to. Without one, it is the cell's private scope.
- Read, write, modify, or borrow the value, at the handle's key or another.

## Example

```rust
use myrmic_sdk::db::state::State;
use myrmic_sdk::db::Scope;
use myrmic_sdk::Metadata;

// No scope, so this value is private to the cell.
const COUNT: State<u64> = State::new_const("count");

// The same type in a public scope, shared with other cells.
const TOTAL: State<u64> = State::new_const_in("total", Scope::public("application-data"));

#[myrmic_sdk::cmd]
fn record(_md: Metadata) -> myrmic_sdk::Result {
    // Reads, changes, and writes in one step, starting from zero if unset.
    let count = COUNT.upsert_with(|count| *count += 1)?;

    // Borrowed, then written back explicitly, so a failure is not lost.
    let mut total = TOTAL.guard_or_default()?;
    *total += 1;
    total.save()?;

    // The same handle, a different key.
    COUNT.save_to("last-count", &count)?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn report(_md: Metadata) -> myrmic_sdk::Result {
    // An absent key reads as nothing, not as an error.
    let count = COUNT.load()?.unwrap_or_default();

    myrmic_sdk::info!("counted {count}")?;

    Ok(())
}
```

## Behavior

### Normal

A handle fixes the value's type and its scope. Its key is the one every operation uses unless given another.

The value is encoded and decoded whole, every time it crosses into the runtime. Changing a value in place is therefore a read, a change, and a write, not an in-database update.

Every storage call a handler makes joins one transaction, which commits when the handler returns successfully and rolls back when it returns an error or traps.

### Errors

Encoding, decoding, storage access, and communication with the runtime can all fail.

### Limits

Reading uses a fixed buffer of 8 KiB, so a value larger than that cannot be read back. Writing has no such limit, so a cell can store a value it can no longer read.

## API documentation

See the API documentation for [`myrmic_sdk::db::state`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/state/index.html), which covers every operation on a state handle, and its guard.
