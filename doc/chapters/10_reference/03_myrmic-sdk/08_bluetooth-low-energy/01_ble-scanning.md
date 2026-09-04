# BLE scanning and discovery

> **Availability:** Linux and embedded runtimes, on a node with Bluetooth hardware

Scanning finds nearby Bluetooth peripherals. The runtime listens for their advertisements and delivers each one to a handler the cell names. Filtering is optional.

Scanning does not block the cell's handler and advertisements arrive at a specified handler as they are found.

## When to use

Use scanning when a cell has to find a peripheral before connecting to it, or reads what a peripheral advertises without connecting at all.

Connect directly when the peripheral's address is already known.

## Operations

- Start a scan, and stop it.
- Filter by company, by exact local name, or by service UUID.
- Scan passively or actively.
- Receive each advertisement in a handler, with the peripheral's address and what it advertised.

## Example

```rust
use myrmic_sdk::ble::{DiscoveredDevice, DiscoveryFilter, ScanHandle, ScanMode};
use myrmic_sdk::{Callback, InMemory, Metadata};

// The scan outlives the invocation that started it, so its handle is kept in
// the cell's memory.
static SCAN: InMemory<Option<ScanHandle>> = InMemory::empty();

fn start_scan() -> myrmic_sdk::Result {
    // Every condition set has to match for a peripheral to be reported.
    let filter = DiscoveryFilter {
        company_id: Some(0x0499),
        local_name: Some("RuuviTag Pro".try_into()?),
        service_uuid: None,
    };

    // Active, so peripherals are asked for a scan response as well.
    let handle = myrmic_sdk::ble::scan(
        Callback::of::<device_found>(),
        Some(filter),
        ScanMode::Active,
    )?;

    SCAN.with(|slot| *slot = Some(handle))?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn device_found(_md: Metadata, device: DiscoveredDevice) -> myrmic_sdk::Result {
    myrmic_sdk::info!("found {}", device.address)?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn stop_scan(_md: Metadata) -> myrmic_sdk::Result {
    // Taken out of memory first, so a late advertisement finds nothing to stop.
    if let Some(handle) = SCAN.with(Option::take)? {
        handle.stop()?;
    }

    Ok(())
}
```

## Behavior

### Normal

The runtime applies the filter, so only matching peripherals reach the cell. A local name has to match exactly, including its case.

A passive scan receives only the primary advertisement. An active scan also asks each peripheral for a scan response, which carries what did not fit in the primary one.

A cell runs one scan at a time, and starting another replaces it. Stopping the scan is the cell's job: dropping the handle leaves the scan running.

On the embedded runtime a scan and a connection cannot run at the same time. Connecting stops the scan, and starting a scan closes the connection.

### Errors

Scanning fails when the runtime cannot use the Bluetooth adapter.

### Limits

The Linux runtime supports active scanning only, so a cell that asks for a passive scan gets an active one.

An advertisement is bounded, and anything past the bound is dropped rather than reported: a local name of 32 bytes, 4 service UUIDs each 16-bit or 128-bit, 27 bytes of manufacturer data, and 27 bytes of service data.

An advertisement is also dropped when the cell's queue is full, so a slow handler loses reports.

Stopping does not discard advertisements already queued, so the handler can still run afterwards.

A handle does not identify its scan. Stopping with an old handle stops whichever scan is running now.

## API documentation

For the filter and scan mode type, see [`scan`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/fn.scan.html), [`ScanHandle`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/ble/struct.ScanHandle.html), [`DiscoveryFilter`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/struct.DiscoveryFilter.html) and [`ScanMode`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/enum.ScanMode.html).
