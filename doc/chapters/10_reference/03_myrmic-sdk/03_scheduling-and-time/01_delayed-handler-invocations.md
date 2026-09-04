# Delayed handler invocations

> **Availability:** Linux and embedded runtimes

A cell can schedule a delayed invocation of one of its command handlers. The handler that schedules it carries on and returns as usual, and nothing is blocked while the time passes.

## When to use

Use a delayed invocation for timeouts, deferred cleanup, and one-time retries.

Use a periodic timer when the work should repeat rather than happen once.

## Operations

- Schedule a delayed invocation of a command handler.
- Keep it, so a later invocation can cancel it.
- Cancel one that has not run yet.

## Example

```rust
use core::time::Duration;
use myrmic_sdk::db::state::State;
use myrmic_sdk::{Callback, Metadata, TimerHandle};

// It outlives the invocation that scheduled it, so it is stored.
const TIMEOUT: State<TimerHandle> = State::new_const("timeout");

#[myrmic_sdk::cmd]
fn arm(_md: Metadata) -> myrmic_sdk::Result {
    // Naming the handler is what decides which one is invoked.
    let timer = myrmic_sdk::delay(
        Callback::of::<on_timeout>(),
        Duration::from_secs(30),
    )
    .build()?;

    TIMEOUT.save(&timer)
}

#[myrmic_sdk::cmd]
fn stop(_md: Metadata) -> myrmic_sdk::Result {
    if let Some(timer) = TIMEOUT.load()? {
        // Cancelling uses it up, so it cannot be cancelled twice.
        timer.cancel()?;
    }

    Ok(())
}

#[myrmic_sdk::cmd]
fn on_timeout(_md: Metadata) -> myrmic_sdk::Result {
    myrmic_sdk::warn!("timeout expired")?;

    Ok(())
}
```

## Behavior

### Normal

Once the time has passed, the runtime calls the named handler as a fresh invocation, with the usual metadata and no payload. Only a command handler that takes no payload can be called this way.

The delayed invocated handler cannot see anything from the handler that scheduled it. Whatever it needs must be stored first.

Scheduling takes effect immediately. If that handler then fails, its writes and messages are rolled back, but the schedule stays.

### Errors

Scheduling fails when the cell has no handler of that name, or when it already has five scheduled invocations.

Cancelling fails when the invocation has already run or has already been cancelled.

### Limits

A cell may have five scheduled invocations at once. Delayed and periodic ones share that count.

Delays are rounded down to whole milliseconds, so anything under a millisecond runs immediately.

Dropping the handle does not cancel the invocation, and once it has run there is nothing left to cancel.

## API documentation

For exact signatures and the handle's methods, see [`delay`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.delay.html) and [`TimerHandle`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/struct.TimerHandle.html).
