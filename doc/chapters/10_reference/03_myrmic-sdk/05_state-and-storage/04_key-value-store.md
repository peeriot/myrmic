# Key-value store

> **Availability:** Linux and embedded runtimes

A key-value store keeps many values of one type beneath a shared prefix. Each value has its own key below that prefix.

## When to use

Use a key-value store for a collection whose names are hierarchical and worth scanning, such as one entry per device.

Use a table when entries need explicit identifiers, a count, or ordered traversal.

## Operations

- Declare a typed store under a prefix, fixed at compile time, and which scope it belongs to.
- Store a value, read it, or remove it.
- Collect the keys under a prefix without reading their values.
- Read the values under a prefix, with a callback, as an iterator, or all at once.

## Example

```rust
use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::Metadata;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct DeviceConfig {
    enabled: bool,
}

const CONFIGS: Kv<DeviceConfig> = Kv::new("configs");

#[myrmic_sdk::cmd]
fn enable(_md: Metadata) -> myrmic_sdk::Result {
    let config = DeviceConfig { enabled: true };

    // A slash joins the prefix to the key, so this is stored under
    // "configs/warehouse-a/sensor-01".
    CONFIGS.put("warehouse-a/sensor-01", &config)?;

    // A callback over a narrower prefix. Every key is loaded first, then the
    // values are lazy loaded one at a time. A value that cannot be read or
    // decoded is skipped and nothing is reported.
    CONFIGS.for_each("warehouse-a", |config| {
        let _ = config;
    })?;

    // An iterator instead. It lazy loads the same way, and each failure reaches
    // the caller.
    for value in CONFIGS.iter("warehouse-a")? {
        let _config = value?;
    }

    // Nothing is lazy loaded here: every value is held in memory at once.
    let _all = CONFIGS.list("warehouse-a")?;

    // The keys alone, all of them in memory, without reading any value. Either
    // way, a key removed while reading is skipped.
    let _keys = CONFIGS.keys("warehouse-a")?;

    Ok(())
}
```

## Behavior

### Normal

A store fixes the value's type, its prefix, and its scope.

An empty prefix selects everything in the store, and nothing outside it.

### Errors

Storage access, encoding, decoding, and communication with the runtime can all fail.

### Limits

Each value is read through a fixed buffer of 8 KiB, and a listing of keys through one of 16 KiB, so anything larger cannot be read back. Writing has no such limit.

## API documentation

See the API documentation for [`myrmic_sdk::db::tree`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/tree/index.html), which covers every store operation and its iterator.
