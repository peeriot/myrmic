# BLE connections and service discovery

> **Availability:** Linux and embedded runtimes, on a node with Bluetooth hardware

A connection links a cell to a Bluetooth peripheral. It carries the peripheral's address and the GATT services and characteristics found while connecting.

## When to use

Use a connection when a cell has to read, write, or subscribe to a peripheral's characteristics.

Scan first when the address is not known yet.

## Operations

- Start a connection to an address, naming a handler for success and one for failure.
- Look up a characteristic by its service UUID and its own UUID.
- Read the connected peripheral's address.
- Disconnect.

## Example

```rust
use myrmic_sdk::ble::{uuid128, Address, Connection, Disconnect, Uuid};
use myrmic_sdk::types::ble::DisconnectReason;
use myrmic_sdk::{Callback, InMemory, Metadata};

/// Nordic NUS service, and the characteristic the peripheral sends on.
const NUS_SERVICE: Uuid = uuid128!("6E400001-B5A3-F393-E0A9-E50E24DCCA9E");
const TX_CHAR: Uuid = uuid128!("6E400003-B5A3-F393-E0A9-E50E24DCCA9E");

static CONNECTION: InMemory<Option<Connection>> = InMemory::empty();

fn connect(address: Address) -> myrmic_sdk::Result {
    myrmic_sdk::ble::connect(address)
        .on_connected(Callback::of::<connected>())
        .on_disconnected(Callback::of::<disconnected>())
        // Returns once the attempt has started. The outcome arrives at one of
        // the two handlers above.
        .initiate()?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn connected(_md: Metadata, connection: Connection) -> myrmic_sdk::Result {
    // A characteristic is named by two UUIDs: its service, and its own.
    let _characteristic = connection
        .characteristic(NUS_SERVICE, TX_CHAR)
        .ok_or("the peripheral has no such characteristic")?;

    // Reads, writes, and subscriptions all need this, so it is kept.
    CONNECTION.with(|slot| *slot = Some(connection))?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn stop(_md: Metadata) -> myrmic_sdk::Result {
    // This does not trigger the `on_disconnected` handler.
    if let Some(connection) = CONNECTION.with(Option::take)? {
        connection.disconnect()?;
    }

    Ok(())
}

#[myrmic_sdk::cmd]
fn disconnected(_md: Metadata, disconnect: Disconnect) -> myrmic_sdk::Result {
    match disconnect.reason() {
        // The attempt never got a link.
        DisconnectReason::ConnectionFailed => myrmic_sdk::warn!("could not connect")?,
        // The peripheral closed the link.
        DisconnectReason::RemoteClosed => myrmic_sdk::warn!("the peripheral left")?,
        // This node closed it, without the cell asking.
        DisconnectReason::LocalClosed => myrmic_sdk::warn!("the node closed the link")?,
        other => myrmic_sdk::warn!("disconnected: {other:?}")?,
    }

    CONNECTION.with(|slot| *slot = None)?;

    Ok(())
}
```

## Behavior

### Normal

Connecting does not block the cell's handler. The call returns immediately and the outcome arrives later: the specified success handler runs when the link is up and its services are ready, and the specified failure handler runs when the attempt fails or a live link drops.

The services and characteristics are found while connecting, and a lookup searches only those.

### Errors

Starting fails when either handler is missing, and when the runtime cannot use the Bluetooth adapter.

A failure after that point reaches the failure handler with a reason: the attempt failed, the peripheral closed the link, or the cell's own node closed it.

### Limits

Letting go of a connection does not disconnect the peripheral.

A connection stops working when the runtime restarts.

The embedded runtime allows one connection at a time on a node, and refuses another. The Linux runtime allows any number.

On the embedded runtime, starting a scan closes an open connection, and the failure handler runs with the reason that the cell's own node closed it.

Some runtimes require a scan before a connection, and refuse a direct attempt. Scanning first works everywhere.

## API documentation

For every connection method and disconnect reason, see [`connect`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/fn.connect.html), [`ConnectBuilder`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/struct.ConnectBuilder.html), [`Connection`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/struct.Connection.html) and [`Disconnect`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/struct.Disconnect.html).
