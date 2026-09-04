# BLE pairing

> **Availability:** Linux and embedded runtimes, on a node with Bluetooth hardware

Some peripherals keep their characteristics behind a secured link. Pairing with a passkey secures it.

## Operations

- Pair with a passkey.

## Example

```rust
use myrmic_sdk::ble::{mac_addr_pub, Address, Connection, Disconnect};
use myrmic_sdk::{Callback, Metadata};

const SENSOR: Address = mac_addr_pub!("C0:98:E5:42:7A:11");

fn connect_sensor() -> myrmic_sdk::Result {
    myrmic_sdk::ble::connect(SENSOR)
        .on_connected(Callback::of::<connected>())
        .on_disconnected(Callback::of::<disconnected>())
        .initiate()?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn connected(_md: Metadata, _connection: Connection) -> myrmic_sdk::Result {
    // Pairing needs the connection, so it happens here and not before.
    myrmic_sdk::ble::set_pair_passkey(123_456)?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn disconnected(_md: Metadata, _reason: Disconnect) -> myrmic_sdk::Result {
    Ok(())
}
```

## Behavior

### Normal

Pairing happens immediately, on the connections that already exist. The embedded runtime pairs its one connection, and the Linux runtime pairs every connection the cell holds.

### Errors

Pairing fails when:

- no peripheral is connected
- the runtime cannot register the passkey
- the peripheral rejects the pairing, as when the passkey is wrong

### Limits

On the Linux runtime one passkey is stored for the whole node. Any cell that sets a passkey overwrites the stored value, and every pending pairing uses the new one.

## API documentation

For the pairing function, see [`set_pair_passkey`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/fn.set_pair_passkey.html).
