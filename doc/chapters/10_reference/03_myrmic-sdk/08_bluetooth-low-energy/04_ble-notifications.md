# BLE notifications and subscriptions

> **Availability:** Linux and embedded runtimes, on a node with Bluetooth hardware

With a subscription, a connected peripheral pushes each new value of a characteristic instead of the cell polling it. Subscribing registers a handler, and each value is delivered to it.

## When to use

Subscribe for values the peripheral produces on its own.

Read the characteristic instead when the cell needs a value only occasionally.

## Operations

- Subscribe to a characteristic already looked up on a connection.
- Receive each value in a handler, with the characteristic it came from.
- Unsubscribe.

## Example

```rust
use myrmic_sdk::ble::{Characteristic, Connection, Notification, Subscription};
use myrmic_sdk::{Callback, InMemory, Metadata};

// Unsubscribing later needs this, so it outlives the handlers invocation.
static SUBSCRIPTION: InMemory<Option<Subscription>> = InMemory::empty();

fn subscribe(connection: &Connection, characteristic: Characteristic) -> myrmic_sdk::Result {
    let subscription = connection
        .subscribe(characteristic, Callback::of::<notification>())?
        .map_err(|_| "the peripheral refused the subscription")?;

    SUBSCRIPTION.with(|slot| *slot = Some(subscription))?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn notification(_md: Metadata, notification: Notification) -> myrmic_sdk::Result {
    // Each notification carries the characteristic it came from, so one handler
    // can serve several.
    myrmic_sdk::info!(
        "{:?}: {} bytes",
        notification.characteristic(),
        notification.data().len()
    )?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn stop(_md: Metadata) -> myrmic_sdk::Result {
    // Taken out of memory first, so a late value finds nothing to unsubscribe.
    if let Some(subscription) = SUBSCRIPTION.with(Option::take)? {
        subscription.unsubscribe()?;
    }

    Ok(())
}
```

## Behavior

### Normal

Subscribing blocks the cell's handler until the subscription is set up. The values arrive later.

Each value the peripheral sends triggers the handler, carrying the characteristic and the raw bytes.

Values stop arriving when the cell unsubscribes and when the connection ends.

### Errors

Subscribing fails when:

- the connection or the characteristic is gone
- the characteristic cannot be subscribed to
- the characteristic needs a paired link first
- the runtime has no room for another subscription

### Limits

Unsubscribing is the cell's job: dropping the handle leaves the subscription running.

The embedded runtime allows eight subscriptions on a node. The Linux runtime sets no limit.

The embedded runtime drops a value when the cell's queue is full.

The Linux runtime holds the value until there is space in the cell's queue, which delays the values behind it.

Unsubscribing does not discard values already queued, so the notification handler can still run afterwards.

## API documentation

For the subscribe function signature and the notification type, see [`Connection`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/struct.Connection.html), [`Subscription`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/struct.Subscription.html) and [`Notification`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/struct.Notification.html).
