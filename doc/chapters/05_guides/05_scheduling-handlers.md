# Schedule Handlers

A cell is event-driven - it waits for a command or event to arrive, handles it, then goes idle. Cell handlers run on a single thread, so while a handler is running, the cell cannot process other incoming messages. An infinite loop inside a handler - polling a sensor, for example - would block the cell entirely. For this reason, Myrmic provides the possibility to schedule handlers to run code at a fixed interval or after a delay.

The Myrmic SDK provides the means a cell needs to schedule its own handlers and run logic on a schedule. All work the same way:

- create a timer
- point it at the handler that should fire
- store the handle you get back - it is the only way to cancel later
- the runtime calls the handler when the time comes

They differ only in when and how often the handler fires.

This guide covers each scheduling option and shows how to use it.

## Run on a fixed period

The first scheduling option fires a handler at a fixed period and keeps going until cancelled or the cell is torn down. It is the right choice for any work that needs to happen on a regular basis - polling a sensor, sending a heartbeat, refreshing a value.

The following example starts a timer that fires every 5 seconds:

```rust
use core::time::Duration;
use myrmic_sdk::db::state::State;

const TIMER: State<myrmic_sdk::TimerHandle> = State::new_const("timer");

#[myrmic_sdk::cmd]
fn start_polling(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    let handle = myrmic_sdk::interval(
        myrmic_sdk::Callback::of::<on_tick>(), // or: Callback::<Void>::to("on_tick")?
        Duration::from_secs(5),
    ).build()?;

    TIMER.save(&handle)?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn on_tick(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("tick").ok();

    Ok(())
}
```

The SDK provides `interval`, it takes:

- a callback - names the handler that fires on each tick
- a period - sets how often it fires

It returns a builder. Call `.build()` to create the timer and get back a handle.

The handle:

- must be stored so it survives across invocations
- dropping it without storing does not stop the timer - it is the only way to cancel later.

The handler named in the callback must:

- be annotated with `#[myrmic_sdk::cmd]` - scheduled handlers fire through the same dispatch mechanism as commands
- take only `Metadata` - should not carry any payload

In case the handler needs to run a fixed number of times rather than indefinitely, call `.count(n)` on the builder before `.build()`:

```rust
myrmic_sdk::interval(callback, Duration::from_millis(500))
    .count(3)
    .build()?;
```

After the last tick, the timer stops and the stored handle is no longer valid.

In case the handler should not start immediately, `interval_at` adds an initial delay before the first tick:

```rust
myrmic_sdk::interval_at(
    callback,
    Duration::from_secs(10), // initial delay before the first tick
    Duration::from_secs(5),  // period after that
).build()?;
```

## Run once after a delay

The other option fires a handler once after a fixed duration, then stops. It is the right choice for work that should happen after a wait - sending an alert if no response arrives, triggering a one-time action after a delay.

The following example arms a watchdog that fires after 30 seconds:

```rust
use core::time::Duration;
use myrmic_sdk::db::state::State;

const TIMEOUT: State<myrmic_sdk::TimerHandle> = State::new_const("timeout");

#[myrmic_sdk::cmd]
fn arm_watchdog(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    let handle = myrmic_sdk::delay(
        myrmic_sdk::Callback::of::<on_timeout>(), // or: Callback::<Void>::to("on_timeout")?
        Duration::from_secs(30),
    ).build()?;

    TIMEOUT.save(&handle)?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn on_timeout(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::warn!("watchdog expired").ok();

    Ok(())
}
```

The SDK provides `delay`, it takes:

- a callback - names the handler that fires
- a duration - how long to wait before firing

It returns a builder. Call `.build()` to create the timer and get back a handle.

After the handler fires, the handle in state is no longer valid - the timer has already run and there is nothing left to cancel.

## Cancelling a timer

As seen in the previous examples, the handle is always stored in state. Storing it serves one purpose - being able to stop whatever was scheduled when the need arises, since dropping it does not stop it - the scheduled handlers keep running with no way to cancel.

The handle is of type `TimerHandle` - it exposes `.cancel()`, the only way to stop whatever was scheduled. The following example stops the polling interval:

```rust
#[myrmic_sdk::cmd]
fn stop_polling(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    if let Some(handle) = TIMER.load()? {
        handle.cancel()?;
    }

    Ok(())
}
```

## Event and command dispatch from a scheduled handler

Dispatching commands and events from a scheduled handler is non-blocking - execution continues immediately after each call. All commands and events dispatched from a scheduled handler commit atomically together with any storage operations when the handler completes - nothing is visible to the outside until that point.

## See also

- [How to work with commands](./02_commands.md)
- [How to publish and handle events](./03_events.md)
- [State and Storage](./06_state-and-storage.md) - working with state and storage
- [`myrmic send` reference](../10_reference/02_myrmic-cli/08_send.md)

## Related SDK reference

- [Delayed handler invocations](../10_reference/03_myrmic-sdk/03_scheduling-and-time/01_delayed-handler-invocations.md)
- [Periodic handler invocations](../10_reference/03_myrmic-sdk/03_scheduling-and-time/02_periodic-handler-invocations.md)
- [Clock, uptime, and pausing](../10_reference/03_myrmic-sdk/03_scheduling-and-time/03_clock-and-wait.md)
