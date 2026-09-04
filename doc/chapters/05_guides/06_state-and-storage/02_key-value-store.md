# Key-value Store

`Kv<V>` is the SDK abstraction for storing many typed values under different keys beneath one prefix. This page explains each operation in detail.

## Declare

A store is declared as a handle bound to a prefix and a value type. Declare it as a module-level constant:

```rust
use myrmic_sdk::db::tree::Kv;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct DeviceConfig {
    // ...
}

const CONFIGS: Kv<DeviceConfig> = Kv::new("configs");
```

This binds to the default scope - private to the cell. To use a custom scope, pass it explicitly using `new_in`:

```rust
const CONFIGS: Kv<DeviceConfig> = Kv::new_in("configs", Scope::public("my-app"));
```
Every key in the store is rooted at the prefix - the full stored key is `{prefix}/{key}`, for example `configs/sensor-01`.

## Write

The store handle exposes a `put` method. It takes a key and a reference to the value, storing it under that key, overwriting any existing value:

```rust
CONFIGS.put("sensor-01", &config)?;
```

## Read

The store handle exposes a `get` method. It returns the value stored under that key, or `None` if nothing is there:

```rust
match CONFIGS.get("sensor-01")? {
    Some(config) => { /* use config */ }
    None => { /* not found */ }
}
```

## Delete

The store handle exposes a `delete` method. It removes the entry under that key:

```rust
CONFIGS.delete("sensor-01")?;
```

## Iterate

The store handle offers three ways to iterate over its entries. All take a `sub` parameter to control which entries are scanned (`""` for all entries, or a sub-key like `"warehouse-a/"` to narrow).

1. `for_each` - applies a closure to each entry:

```rust
CONFIGS.for_each("warehouse-a/", |config| {
    // handle config
})?;
```

Entries that fail to decode are skipped.

2. `iter` - returns a lazy iterator over the entries, surfacing load and decode errors:

```rust
for config in CONFIGS.iter("warehouse-a/")? {
    let config = config?;
    // handle config
}
```

3. `list` - collects all entries into memory at once:

```rust
let configs = CONFIGS.list("warehouse-a/")?;

for config in configs {
    // handle config
}
```

Prefer `for_each` or `iter` for large stores - they process entries one at a time and keep memory use low.

## Keys

The store handle exposes a `keys` method. It returns all stored keys under a sub-prefix - without loading the values:

```rust
let keys = CONFIGS.keys("warehouse-a/")?;

for key in keys {
    // handle key
}
```

Passing an empty string returns all keys in the store.

## See also

- [How to work with state and storage](../06_state-and-storage.md#key-value-store) - back to the guide
- [State](./01_state.md) - one typed value under one fixed key
- [Table store](./03_table-store.md) - a named collection of typed entries

## Related SDK reference

- [Storage scopes](../../10_reference/03_myrmic-sdk/05_state-and-storage/01_storage-scopes.md)
- [Key-value store](../../10_reference/03_myrmic-sdk/05_state-and-storage/04_key-value-store.md)
