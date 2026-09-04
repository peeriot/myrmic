# Clock, uptime, and pausing

> **Availability:** Linux and embedded runtimes

A cell can read the current time, read how long its node has been running, and pause for a duration.

## When to use

Pause only when the running handler cannot proceed for a certain period.

Use a timer to delay invoking a handler, or to invoke it periodically.

## Operations

- Read the current time, as a duration since the Unix epoch.
- Read how long the node has been running.
- Pause for a duration.

## Example

```rust
use core::time::Duration;
use myrmic_sdk::Metadata;

#[myrmic_sdk::cmd]
fn inspect_time(_md: Metadata) -> myrmic_sdk::Result {
    let current_time = myrmic_sdk::now()?;
    let running_for = myrmic_sdk::uptime()?;

    if running_for < Duration::from_secs(1) {
        myrmic_sdk::wait(Duration::from_millis(10))?;
    }

    myrmic_sdk::info!("time {current_time:?}, up for {running_for:?}")?;

    Ok(())
}
```

## Behavior

### Normal

The current time comes from the node's swarm-synchronised clock, so timestamps taken on different nodes can be compared and ordered.

Uptime counts from when the node started and only ever moves forward. It resets when the node restarts, and uptimes from two nodes cannot be compared.

A pause runs for its full duration and then succeeds. Nothing cancels or interrupts it, and nothing limits how long a handler may take.

While a cell is paused it does nothing else. Commands, events, replies, and scheduled invocations all queue behind it.

### Errors

Reading the current time fails on an embedded node whose clock has not yet synchronised with the swarm.

Pausing fails only when the duration is malformed.

## API documentation

See [`now`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.now.html), [`uptime`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.uptime.html) and [`wait`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.wait.html) in the API documentation.
