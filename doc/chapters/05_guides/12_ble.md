# Work with BLE peripherals

In some use cases, a cell needs data from a device that communicates exclusively over Bluetooth Low Energy - a sensor tag, a beacon, or any device that advertises and exposes GATT services. The Myrmic SDK gives the cell model the tools to interact with these devices through the `ble` module (`myrmic_sdk::ble`).

It provides:

- **Discovery** - scanning for peripherals and filtering advertisements
- **Connection** - connecting to a peripheral and managing its lifecycle
- **Data exchange** - reading, writing, and subscribing to GATT characteristics
- **Security** - pairing with peripherals that require a secured link

This guide covers each of these in practice and shows how to use them in a cell, with code snippets throughout.

> **Note:** This guide is about a cell interacting with a BLE peripheral that is not part of the swarm - ot about two Myrmic Nodes communicating over BLE.

## Placement requirement

A BLE radio only reaches devices within its own range - the device running the cell and the peripheral must be physically within range of each other.

This also means the cell must run on the specific device that has a BLE radio. This requirement is expressed with capability tags:

- **Runtime** - each runtime advertises its capabilities through tags set at startup time - for example `ble`, `gps`, or `camera` - describing what the device it runs on has to offer:

  ```bash
  myrmic runtimes start --tag ble
  ```

- **Cell** - a cell declares what it needs to fulfil its function through tags at deploy time. The distributed swarm ensures the cell lands on a device that has the required capabilities to run it, and therefore the right hardware.

  For a single cell, pass the tag directly to the deploy command:

  ```bash
  myrmic deploy my-cell.wasm --tag ble
  ```

  For a full application, declare it per instance in `app_specs.yml`:

  ```yaml
  instances:
    - class: my-cell
      tags: [ble]
  ```

  Then deploy with:

  ```bash
  myrmic deploy app_specs.yml
  ```

For more details, explanations, and examples see:

- [Cell execution and placement](./01_cells.md#cell-execution-and-placement) - how placement tags work in practice
- [Cell and application configuration](../10_reference/01_configuration/02_cell-and-application-configuration.md) - the full `app_specs.yml` schema
- [`myrmic runtimes start`](../10_reference/02_myrmic-cli/04_runtimes/01_start.md) - starting a runtime with capability tags
- [`myrmic deploy`](../10_reference/02_myrmic-cli/05_deploy.md) - deploying cells and applications with placement tags

## Non-blocking by design

Cell handlers (commands, events, scheduled handlers and initialization handler) run on a single thread - this means while a handler is running, the cell cannot process other incoming messages. But some BLE interactions - scanning, connecting, reading a characteristic - take time. Waiting for the result inside a cell handler would block the thread and the cell would be unable to handle any other incoming command or event until the interaction completes.

For this reason, every operation the SDK provides is non-blocking. The cell starts the operation, registers a callback, and returns immediately. The runtime invokes that callback when the result arrives.

A callback names the handler that fires when the result is ready. The handler:

- must be annotated with `#[myrmic_sdk::cmd]` - callbacks fire through the same dispatch mechanism as commands
- must take the result type for that BLE operation as a parameter

```rust
use myrmic_sdk::{Callback, Metadata, Result};
use myrmic_sdk::ble::{scan, DiscoveredDevice, ScanHandle};

// start the scan and name the handler to invoke when a device is found
let scan_handle: ScanHandle = scan(Callback::of::<on_device_found>(), filter, mode)?;

// the runtime invokes this handler when a matching device is discovered
#[myrmic_sdk::cmd]
fn on_device_found(_md: Metadata, device: DiscoveredDevice) -> Result<()> { /* ... */ }
```

## Handles

Some operations outlive the cell handler that started them - for example a scan that keeps listening for peripherals, a connection that stays up, or a subscription that keeps delivering notifications. To track and control them after that handler returns, the SDK functions that start these operations return a handle.

```rust
use myrmic_sdk::{Callback, Metadata, Result};
use myrmic_sdk::ble::{connect, scan, Connection, ScanHandle, Subscription};

let scan_handle: ScanHandle = scan(on_device_found_callback, filter, mode)?;

connect(address)
    .on_connected(on_connected_callback)
    .on_disconnected(on_disconnected_callback)
    .initiate()?;

#[myrmic_sdk::cmd]
fn on_connected(_md: Metadata, connection_handle: Connection) -> Result<()> {
    let subscription_handle: Subscription = connection_handle
        .subscribe(characteristic, on_notification_callback)?
        .map_err(|_| "not notifiable")?;
}
```

Each handle gives you control over what was started:

- `ScanHandle` - stop the scan
- `Connection` - interact with the peripheral and close the connection when done
- `Subscription` - stop notification delivery

A handle must be stored and persisted in the runtime database - not in a local variable inside a cell handler, since when the handler returns everything it held in memory is gone. Dropping a handle does **not** clean up the resource it represents - the scan keeps running, the connection stays up, the subscription keeps delivering. Without the handle there is no way to stop, disconnect, or unsubscribe.

> **Note:** Always free the resources held by handles when they are no longer needed - unsubscribe from notifications, close the connection, then stop scanning. Leaving them open keeps radio resources occupied.

See [State and storage](./06_state-and-storage.md) for how to persist handles in the runtime database.

## Scan for a peripheral

A scan runs continuously - once started, it listens for BLE advertisements and delivers each matching device to a specified cell handler as it is found. It keeps going until explicitly stopped. It can be narrowed by a filter and configured for the scan mode needed.

It is the right starting point for any cell that needs to discover a peripheral before connecting, or that reads data directly from the advertisement without connecting at all.

The following example starts a scan and registers a handler for the runtime to call whenever a peripheral is found:

```rust
use myrmic_sdk::db::state::State;
use myrmic_sdk::{Callback, Metadata, Result};
use myrmic_sdk::ble::{scan, DiscoveredDevice, ScanHandle, ScanMode};

const SCAN: State<Option<ScanHandle>> = State::new_const("scan");

#[myrmic_sdk::cmd]
fn enable(_md: Metadata) -> Result<()> {
    let scan_handle = scan(Callback::of::<on_device_found>(), None, ScanMode::Passive)?;
    SCAN.save(&Some(scan_handle))?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn on_device_found(_md: Metadata, device: DiscoveredDevice) -> Result<()> {
    // ...
    Ok(())
}
```

`on_device_found` fires once per advertisement received. The runtime passes it a `DiscoveredDevice` value carrying the discovered peripheral's address and its advertisement - local name, service UUIDs, manufacturer data, and service data. From there, the cell can use the address to initiate a connection, or inspect the payload to decode readings directly - without connecting at all.

A cell runs one scan at a time - this means a new scan cannot be started until the current one is stopped. So scanning must be stopped as soon as a match is found or it is no longer needed.

```rust
#[myrmic_sdk::cmd]
fn on_device_found(_md: Metadata, device: DiscoveredDevice) -> Result<()> {
    let mut stored = SCAN.load()?.unwrap_or_default();
    let Some(scan_handle) = stored.take() else {
        return Ok(());
    };

    SCAN.save(&stored)?;
    scan_handle.stop()?;

    // ... connect to the peripheral or inspect the advertisement

    Ok(())
}
```

> **Note:** Stopping the scan does not immediately flush queued advertisements - they continue to arrive until the stop takes effect, so the handler may still fire after stopping. For this reason, the scan handle must be cleared from state before stopping, so any stale call finds nothing to act on.

### Filtering

The scan can be narrowed to report only devices that match specific criteria:

- by manufacturer - which company made the peripheral.
- by name - the name the peripheral advertises.
- by service UUID - the Bluetooth service it offers, for example heart rate, temperature, or humidity.

These filtering options can be combined to narrow the scan further - a device must satisfy every condition set to be reported.

```rust
use myrmic_sdk::ble::{scan, DiscoveryFilter, uuid128, ScanMode};

#[myrmic_sdk::cmd]
fn enable(_md: Metadata) -> Result<()> {
    let filter = DiscoveryFilter {
        company_id: Some(0x0499), // Ruuvi Innovations, assigned by Bluetooth SIG
        local_name: Some("RuuviTag Pro".try_into()?),
        // service_uuid accepts Uuid::Bit16(0x180D) for standard 16-bit Bluetooth services,
        // or a 128-bit UUID constructed with the uuid128! macro for proprietary ones
        service_uuid: Some(uuid128!("6E400001-B5A3-F393-E0A9-E50E24DCCA9E")), // Nordic NUS Service
    };

    let scan_handle = scan(Callback::of::<on_device_found>(), Some(filter), ScanMode::Passive)?;

    Ok(())
}
```

### Scan mode

A scan runs in one of two modes - passive or active - depending on whether scan responses need to be requested:

- passive - only primary advertisements (`ADV_IND`) are received. Lower power; sufficient for most peripherals.
- active - sends scan requests to receive scan responses (`SCAN_RSP`) from peripherals that advertise more data than fits in the primary advertisement. Use active mode only when the peripheral puts data you need in the scan response, at the cost of extra radio airtime.

```rust
scan(callback, filter, ScanMode::Passive)?;
// or
scan(callback, filter, ScanMode::Active)?;
```

## Connect to a peripheral

Once a peripheral is found, a connection can be established with it, giving the cell access to its GATT characteristics for reading, writing, and subscribing to notifications.

Here is how that looks in practice:

```rust
use myrmic_sdk::ble::{connect, Connection, Disconnect};

#[myrmic_sdk::cmd]
fn on_device_found(_md: Metadata, device: DiscoveredDevice) -> Result<()> {
    // stop the scan first ...

    connect(device.address)
        .on_connected(Callback::of::<on_connected>())
        .on_disconnected(Callback::of::<on_disconnected>())
        .initiate()?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn on_connected(_md: Metadata, connection: Connection) -> Result<()> {
    // ...
    Ok(())
}

#[myrmic_sdk::cmd]
fn on_disconnected(_md: Metadata, disconnect: Disconnect) -> Result<()> {
    // ...
    Ok(())
}
```

`connect` takes the peripheral's address - obtained from the scan - and returns a connection builder instance. Two callbacks must be registered on it before trying to initiate the connection:

- `on_connected` - fires when the link is up and GATT services are ready, delivering a `Connection` handle for reading, writing, subscribing to characteristics, and disconnecting when done.
- `on_disconnected` - fires when the connection attempt fails or the connection link drops.

### Connection established

When the link is up and GATT services are ready, `on_connected` receives a `Connection` handle. Through it the cell can read the peripheral's address, look up characteristics, and close the connection when done:

```rust
use myrmic_sdk::ble::{Uuid, uuid128};

/// Nordic NUS Service
const NUS_SERVICE: Uuid = uuid128!("6E400001-B5A3-F393-E0A9-E50E24DCCA9E");
/// Characteristic for TX
///
/// This characteristic is used to receive data from the peripheral
const TX_CHAR: Uuid = uuid128!("6E400003-B5A3-F393-E0A9-E50E24DCCA9E");

#[myrmic_sdk::cmd]
fn on_connected(_md: Metadata, connection: Connection) -> Result<()> {
    // a characteristic is identified by two UUIDs: the service it belongs to and its own
    let characteristic = connection
        .characteristic(NUS_SERVICE, TX_CHAR)
        .ok_or("peripheral is missing the expected characteristic")?;

    // connection.disconnect(); // close the connection when the peripheral is no longer needed

    Ok(())
}
```

### Connection failure

`on_disconnected` fires whenever the connection ends or the connection attempt fails for some reason. Inspect the reason to know what happened and decide what to do next:

```rust
use myrmic_sdk::types::ble::DisconnectReason;

#[myrmic_sdk::cmd]
fn on_disconnected(_md: Metadata, disconnect: Disconnect) -> Result<()> {
    match disconnect.reason() {
        DisconnectReason::ConnectionFailed => {} // the connection attempt failed
        DisconnectReason::Timeout          => {} // the connection attempt timed out
        DisconnectReason::RemoteClosed     => {} // the peripheral closed the connection
        DisconnectReason::LocalClosed      => {} // the cell closed the connection
        DisconnectReason::Unknown          => {} // the connection ended for an unspecified reason
    }

    Ok(())
}
```

### Connecting to a known address

When the peripheral's address is already known - from a previous session or hardcoded - a connection can be initiated directly without scanning first:

```rust
use myrmic_sdk::ble::{connect, mac_addr_pub, Address};

const PERIPHERAL: Address = mac_addr_pub!("C0:98:E5:42:7A:11");

#[myrmic_sdk::cmd]
fn connect_known_address(_md: Metadata) -> Result<()> {
    connect(PERIPHERAL)
        .on_connected(Callback::of::<on_connected>())
        .on_disconnected(Callback::of::<on_disconnected>())
        .initiate()?;
    Ok(())
}
```

- `mac_addr_pub!` - for public addresses, assigned by the manufacturer and globally unique.
- `mac_addr_rand!` - for random addresses, generated by the device itself.

> **Note:** Some adapters require a prior scan before a connection can be established - skipping it will result in `ConnectionFailed`. To be safe, scan first.

## Pair with a peripheral

Some peripherals protect sensitive data behind an encrypted link. To access their characteristics, the link must first be secured through pairing. Pairing uses a shared passkey - fixed on the peripheral and registered in the cell. The passkey applies to connections that already exist, so it is registered once the peripheral is connected, not before.

```rust
use myrmic_sdk::ble::{connect, set_pair_passkey};

#[myrmic_sdk::cmd]
fn on_device_found(_md: Metadata, device: DiscoveredDevice) -> Result<()> {
    // stop the scan first ...

    connect(device.address)
        .on_connected(Callback::of::<on_connected>())
        .on_disconnected(Callback::of::<on_disconnected>())
        .initiate()?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn on_connected(_md: Metadata, _connection: Connection) -> Result<()> {
    set_pair_passkey(123_456)?; // the passkey is set on the peripheral - find it in its data sheet

    Ok(())
}
```

## Read and write characteristics

Once connected, a characteristic can be read to get its current value, or written to send a new one. Both operations are non-blocking - the request is queued and the outcome delivered to a callback when it completes.

Reading a characteristic:

```rust
use myrmic_sdk::ble::ReadError;
use myrmic_sdk::types::ble::ReadOutcome;

#[myrmic_sdk::cmd]
fn on_connected(_md: Metadata, connection: Connection) -> Result<()> {
    connection.read(characteristic, Callback::of::<on_reading>())?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn on_reading(_md: Metadata, outcome: ReadOutcome) -> Result<()> {
    match outcome.value {
        Ok(bytes)                        => {} // the value as raw bytes
        Err(ReadError::NotReadable)      => {} // the characteristic does not support reading
        Err(ReadError::BufTooSmall)      => {} // internal buffer too small for the value
        Err(ReadError::RequiresSecurity) => {} // the characteristic requires a paired link
    }
    Ok(())
}
```

Writing a characteristic:

```rust
use myrmic_sdk::ble::WriteError;
use myrmic_sdk::types::ble::WriteOutcome;

#[myrmic_sdk::cmd]
fn on_connected(_md: Metadata, connection: Connection) -> Result<()> {
    connection.write(characteristic, &[0x01, 0x00, 0x00, 0x00], Callback::of::<on_written>())?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn on_written(_md: Metadata, outcome: WriteOutcome) -> Result<()> {
    match outcome.result {
        Ok(())                            => {} // write confirmed
        Err(WriteError::NotWriteable)     => {} // the characteristic does not support writing
        Err(WriteError::RequiresSecurity) => {} // the characteristic requires a paired link
    }
    Ok(())
}
```

Both read and write outcomes tell which characteristic they came from - a single handler can serve multiple characteristics:

```rust
fn on_reading(_md: Metadata, outcome: ReadOutcome) -> Result<()> {
    if outcome.characteristic == TEMPERATURE_CHAR {
        // handle temperature
    } else if outcome.characteristic == HUMIDITY_CHAR {
        // handle humidity
    }
    Ok(())
}
```

A write waits for the peripheral to confirm receipt - that is why it takes a callback. Some characteristics do not acknowledge writes at all. For those, `write_no_response` can be used instead - no callback needed:

```rust
connection.write_no_response(characteristic, &[0x01])?;
```

## Subscribe to notifications

Instead of polling a characteristic, a cell can subscribe to it and receive each new value as it arrives - the peripheral pushes updates whenever the value changes.

To subscribe, pass the characteristic and a callback. Each notification is delivered to that callback:

```rust
use myrmic_sdk::ble::{Notification, NotifyError, Subscription};

#[myrmic_sdk::cmd]
fn on_connected(_md: Metadata, connection: Connection) -> Result<()> {
    let subscription = match connection
        .subscribe(characteristic, Callback::of::<on_notification>())?
    {
        Ok(sub)                             => sub,
        Err(NotifyError::NotNotifiable)     => return Ok(()), // characteristic does not support notifications
        Err(NotifyError::RequiresSecurity)  => return Ok(()), // characteristic requires a paired link
        Err(NotifyError::BufTooSmall)       => return Ok(()), // internal buffer too small
    };

    // persist the subscription handle to state

    Ok(())
}

#[myrmic_sdk::cmd]
fn on_notification(_md: Metadata, notification: Notification) -> Result<()> {
    let data           = notification.data();           // the value as raw bytes
    let characteristic = notification.characteristic(); // which characteristic sent it

    Ok(())
}
```

Make sure to unsubscribe when notifications are no longer needed:

```rust
subscription.unsubscribe()?;
```

For complete working examples, the Myrmic repository on GitHub includes several BLE adapter cells that put everything in this guide into practice - see [BLE adapter examples](https://github.com/peeriot/myrmic/tree/master/examples/ble).

## See also

- [How to work with commands](./02_commands.md) - the `Callback` mechanism used throughout this module
- [How to schedule handlers](./05_scheduling-handlers.md) - the same handle-must-be-persisted pattern
- [How to work with state and storage](./06_state-and-storage.md) - persisting handles across invocations

## Related SDK reference

- [BLE scanning and discovery](../10_reference/03_myrmic-sdk/08_bluetooth-low-energy/01_ble-scanning.md)
- [BLE connections and service discovery](../10_reference/03_myrmic-sdk/08_bluetooth-low-energy/02_ble-connections.md)
- [BLE characteristic reads and writes](../10_reference/03_myrmic-sdk/08_bluetooth-low-energy/03_ble-characteristic-io.md)
- [BLE notifications and subscriptions](../10_reference/03_myrmic-sdk/08_bluetooth-low-energy/04_ble-notifications.md)
- [BLE pairing](../10_reference/03_myrmic-sdk/08_bluetooth-low-energy/05_ble-pairing.md)
