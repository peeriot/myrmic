# BLE characteristic reads and writes

> **Availability:** Linux and embedded runtimes, on a node with Bluetooth hardware

A peripheral publishes its data as characteristics, grouped into services, so a characteristic is named by two UUIDs: its service, and its own. A cell reads a characteristic to get its value, and writes one to set it.

Reading and writing do not block the cell's handler. The call returns immediately, and the outcome arrives to a specified handler.

## When to use

Read or write a characteristic once a connection exists and the characteristic has been found.

Subscribe instead when the peripheral should send each new value as it produces it, rather than being asked every time.

## Operations

- Look up a characteristic on a connection.
- Read its value, and receive the outcome in a handler.
- Write a value, and receive the outcome in a handler.
- Write a value without waiting for a confirmation.

## Example

```rust
use myrmic_sdk::ble::{Characteristic, Connection, Uuid};
use myrmic_sdk::types::ble::{ReadOutcome, WriteOutcome};
use myrmic_sdk::{Callback, Metadata};

fn exchange(connection: &Connection, service: Uuid, char_uuid: Uuid) -> myrmic_sdk::Result {
    // Only characteristics found while connecting can be looked up.
    let target: Characteristic = connection
        .characteristic(service, char_uuid)
        .ok_or("characteristic not discovered")?;

    // If the connection is already gone, this returns an error and
    // read_complete never runs.
    connection.read(target, Callback::of::<read_complete>())?;

    connection.write(target, &[0x01], Callback::of::<write_complete>())?;

    // Nothing reports the outcome of this one, including its failure.
    connection.write_no_response(target, &[0x00])?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn read_complete(_md: Metadata, outcome: ReadOutcome) -> myrmic_sdk::Result {
    // The outcome names the characteristic, so one handler can serve several.
    match outcome.value {
        Ok(bytes) => myrmic_sdk::info!("read {} bytes", bytes.len())?,
        Err(error) => myrmic_sdk::error!("read failed: {error:?}")?,
    }

    Ok(())
}

#[myrmic_sdk::cmd]
fn write_complete(_md: Metadata, outcome: WriteOutcome) -> myrmic_sdk::Result {
    if let Err(error) = outcome.result {
        myrmic_sdk::error!("write failed: {error:?}")?;
    }

    Ok(())
}
```

## Behavior

### Normal

Every outcome names the characteristic it came from, so one handler can serve several of them.

A write that reports its outcome waits for the peripheral to confirm it. A write without a confirmation does not, and succeeding only means the runtime took the request.

### Errors

The outcome of a read or a write reports either that the characteristic does not allow it, or that it needs a paired link first.

### Limits

A write without a confirmation reports nothing, not even a refusal.

Neither runtime sets a maximum value size, a deadline, or a retry. A cell that needs a deadline or a retry has to implement it itself.

The embedded runtime handles one Bluetooth request at a time, so the next one waits for it. The Linux runtime runs them concurrently, so their outcomes can reach the handlers in a different order than the calls.

A characteristic belongs to the connection it was looked up on. Once that connection ends, reads and writes on it fail.

## API documentation

For read and write signature, see [`Connection`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/struct.Connection.html) and [`Characteristic`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/struct.Characteristic.html).
