# Time-series store

> **Availability:** Linux and embedded runtimes

The time-series store keeps timestamped measurements. Each sample belongs to a named series and carries tags and typed fields.

## When to use

Use the time-series store for measurements, and observations that are read back by time range.

## Operations

- Publish a measurement into a series, with tags, fields, and an optional timestamp.
- Query one series, narrowing by start time, end time, a result limit, and order.

## Example

```rust
use myrmic_sdk::db::{FieldValue, Measurement, Scope, find_measurement, publish_measurement};
use myrmic_sdk::Metadata;

#[myrmic_sdk::cmd]
fn record(_md: Metadata, value: f64) -> myrmic_sdk::Result {
    publish_measurement(
        Scope::private(),
        Measurement {
            name: "sensor-readings".into(),
            tags: myrmic_sdk::vec![("room".into(), "kitchen".into())],
            fields: myrmic_sdk::vec![("temperature".into(), FieldValue::F64(value))],
            // Left out, so the runtime stamps the sample as it arrives.
            ts: None,
        },
    )
    .map_err(|_| "publishing the measurement failed")?;

    // The caller passes in both buffers, so their size is the caller's to
    // manage.
    let mut req = [0u8; 256];
    let mut resp = myrmic_sdk::vec![0u8; 8192];

    let found = find_measurement(
        Scope::private(),
        "sensor-readings".into(),
        // The newest ten samples: no bounds, and newest first by default.
        Some(10),
        None,
        None,
        None,
        &mut req,
        &mut resp,
    )
    .map_err(|_| "reading the measurements failed")?;

    myrmic_sdk::info!("{} samples", found.samples.len())?;

    Ok(())
}
```

## Behavior

### Normal

A sample is identified by its series and its timestamp, not by its tags. Publishing that pair again means a query returns only the last sample published.

A query's start time is inclusive and its end time is exclusive. Results come back newest first unless ascending order is asked for, and each sample carries its timestamp, tags, and fields.

### Errors

Encoding, scope validation, storage access, and communication with the runtime can all fail.

### Limits

Publishing encodes the scope, series name, tags, and fields into a single buffer of 100 bytes, and anything over that total fails.

A query reads into buffers the caller passes in, so sizing them is up to the caller. Set a limit, or a start and end time, to keep the result small.

Nothing deletes a sample. There is no delete operation, and a cell's data carries no expiry, so a series grows for as long as the cell keeps publishing.

## API documentation

For exact signatures, and the measurement and field types, see [`publish_measurement`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/fn.publish_measurement.html), [`find_measurement`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/fn.find_measurement.html), [`Measurement`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/struct.Measurement.html) and [`FieldValue`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/enum.FieldValue.html).
