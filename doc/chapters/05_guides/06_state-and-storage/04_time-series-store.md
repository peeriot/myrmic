# Time-series Store

The time-series store holds timestamped measurements. The SDK does not provide an abstraction layer for it - all interactions happen directly through the functions below.

## Write

The SDK provides `publish_measurement`. It takes a scope and a measurement and writes it to the store:

```rust
use myrmic_sdk::db::{publish_measurement, FieldValue, Measurement, Scope};

let measurement = Measurement {
    name: myrmic_sdk::String::from("sensor-readings"),
    tags: myrmic_sdk::vec![],
    fields: myrmic_sdk::vec![
        (myrmic_sdk::String::from("temperature"), FieldValue::F64(22.5)),
        (myrmic_sdk::String::from("humidity"),    FieldValue::F64(60.0)),
        (myrmic_sdk::String::from("unit"),        FieldValue::String(myrmic_sdk::String::from("celsius"))),
    ],
    ts: None,  // None = host timestamps it now
};

publish_measurement(Scope::default(), measurement)?;
```

`Measurement` represents a time-series measurement. It carries the name, fields, tags, and timestamp.

Each field is a `(name, FieldValue)` pair - `FieldValue` supports numeric, string, and boolean values.

The timestamp is optional - when not set, the runtime stamps the measurement with the time it was stored.

## Query

Querying is possible through `find_measurement` - it takes a scope and measurement name, and offers time range filtering, a result limit, and ordering by timestamp. It returns a list of samples:

```rust
use myrmic_sdk::db::{find_measurement, FieldValue, Scope, TsOrderBy};

let mut req_buf = [0u8; 256];
let mut resp_buf = myrmic_sdk::vec![0u8; 8192];

let response = find_measurement(
    Scope::default(),
    myrmic_sdk::String::from("sensor-readings"),
    Some(10),                          // limit; None = no limit
    Some(1_700_000_000_000u64),        // start (ms, inclusive); None = from the beginning
    Some(1_700_003_600_000u64),        // end (ms, exclusive); None = up to now
    Some(TsOrderBy::TimestampDesc),    // TimestampDesc (default) or TimestampAsc; None = default
    &mut req_buf,
    &mut resp_buf,
)?;

for sample in response.samples {
    let temperature = sample.fields.iter().find_map(|(k, v)| match (k.as_str(), v) {
        ("temperature", FieldValue::F64(f)) => Some(*f),
        _ => None,
    });
}
```

## See also

- [How to work with state and storage](../06_state-and-storage.md#time-series-store) - back to the guide
- [Table store](./03_table-store.md) - a named collection of typed entries
- [Semantic store](./05_semantic-store.md) - an RDF triple store queried with SPARQL

## Related SDK reference

- [Storage scopes](../../10_reference/03_myrmic-sdk/05_state-and-storage/01_storage-scopes.md)
- [Time-series store](../../10_reference/03_myrmic-sdk/05_state-and-storage/06_time-series-store.md)
