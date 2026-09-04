# Periodic handler invocations

> **Availability:** Linux and embedded runtimes

A cell can schedule repeated invocations of one of its command handlers, on a period.

## When to use

Use a periodic timer for polling, heartbeats, refreshes, and recurring retries.

Use a delayed invocation when the work should happen once.

## Operations

- Schedule a command handler to run repeatedly.
- Delay the first tick.
- Stop after a fixed number of ticks.
- Cancel the schedule.

## Example

```rust
use core::time::Duration;
use myrmic_sdk::db::state::State;
use myrmic_sdk::{Callback, Metadata, TimerHandle};

// It outlives the invocation that scheduled it, so it is stored.
const POLL: State<TimerHandle> = State::new_const("poll");

#[myrmic_sdk::cmd]
fn start(_md: Metadata) -> myrmic_sdk::Result {
    let handle = myrmic_sdk::interval_at(
        Callback::of::<poll>(),
        // Optional first delay. Plain interval starts at once instead.
        Duration::from_secs(60),
        // The period: one tick every five seconds.
        Duration::from_secs(5),
    )
    // Optional. Those five seconds start when the handler finishes, not when
    // the tick begins.
    .fixed_delay()
    // Optional. Stops after three ticks; without it, repeats until cancelled.
    .count(3)
    .build()?;

    POLL.save(&handle)
}

#[myrmic_sdk::cmd]
fn stop(_md: Metadata) -> myrmic_sdk::Result {
    if let Some(handle) = POLL.load()? {
        // Cancelling uses it up, so it cannot be cancelled twice.
        handle.cancel()?;
    }

    Ok(())
}

#[myrmic_sdk::cmd]
fn poll(_md: Metadata) -> myrmic_sdk::Result {
    myrmic_sdk::info!("poll")?;

    Ok(())
}
```

## Behavior

### Normal

Each tick invokes the handler with the usual metadata and no payload. Only a command handler that takes no payload can be scheduled this way.

The countdown to the next tick starts either when the current tick begins, or when its handler finishes.

Counting from the start keeps a steady rate. If the handler takes longer than the period, invocations pile up in the cell's queue, which holds ten. Once it is full:

- the embedded runtime drops one and skips the periods it missed
- the Linux runtime delivers it late rather than losing it

Counting from the handler leaves a full period between runs, so nothing piles up.

Scheduling takes effect immediately. If that handler then fails, its writes and messages are rolled back, but the schedule stays.

### Errors

Scheduling fails when the cell has no handler of the specified name, when a tick count of zero is asked for, or when it already has five scheduled invocations.

Cancelling fails when the schedule has already finished or has already been cancelled.

### Limits

A cell may have five scheduled invocations at once. Periodic and delayed ones share that count.

Periods and initial delays are rounded down to whole milliseconds, so anything under a millisecond runs immediately.

Dropping the handle does not cancel the schedule, and once a counted schedule has finished there is nothing left to cancel.

## API documentation

For exact signatures and the handle's methods, see [`interval`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.interval.html), [`interval_at`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.interval_at.html) and [`TimerHandle`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/struct.TimerHandle.html).
