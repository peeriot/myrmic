# Table Store

`Table<V>` is the SDK abstraction for the table model - a named collection of typed entries, each identified by a key. This page explains each operation in detail.

## Declare

A table is declared as a handle bound to a name and a value type. Declare it as a module-level constant:

```rust
use myrmic_sdk::db::table::Table;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Device {
    // ...
}

const DEVICES: Table<Device> = Table::new("devices");
```

This binds to the default scope - private to the cell. To bind to a custom scope, pass it explicitly using `new_in`:

```rust
const DEVICES: Table<Device> = Table::new_in("devices", Scope::public("my-app"));
```

## Insert

The table handle exposes an `insert` method. It takes a reference to the value and stores it under a runtime-assigned UUID:

```rust
DEVICES.insert(&device)?;
```

To assign an explicit key, use `insert_with`:

```rust
DEVICES.insert_with("sensor-01", &device)?;
```

## Get by ID

The table handle exposes a `get` method. It returns the entry stored under that key, or `None` if nothing is there:

```rust
match DEVICES.get("sensor-01")? {
    Some(device) => { /* use device */ }
    None => { /* not found */ }
}
```

## Delete

The table handle exposes a `delete` method. It removes the entry under that key:

```rust
DEVICES.delete("sensor-01")?;
```

## Count

The table handle exposes a `count` method. It returns the number of entries in the table:

```rust
let count = DEVICES.count()?;
```

## Iterate

The table offers three ways to iterate over its entries:

1. `for_each` - applies a closure to each entry in ascending key order:

```rust
DEVICES.for_each(|device| {
    // handle device
})?;
```

Entries that fail to decode are skipped.

Use `for_each_rev` to iterate in descending order.

2. `iter` - returns a lazy iterator over `(key, value)` pairs, surfacing decode errors:

```rust
for entry in DEVICES.iter() {
    let (key, device) = entry?;
    // handle key and device
}
```

Use `iter_rev` to iterate in descending order.

3. `list` - collects all table entries into memory at once:

```rust
let devices = DEVICES.list()?;

for device in devices {
    // handle device
}
```

Use `list_rev` to collect in descending order.

4. `to_map` - collects the full table into a `BTreeMap<K, V>`:

```rust
let map = DEVICES.to_map()?;

for (key, device) in &map {
    // handle key and device
}
```

Prefer `for_each` or `iter` for large tables - they process entries one at a time and keep memory use low.

## Keys

The table handle offers two ways to access its keys:

1. `keys` - returns a lazy iterator over keys in ascending order, surfacing errors:

```rust
for key in DEVICES.keys() {
    let key = key?;
    // handle key
}
```

Use `keys_rev` to iterate in descending order.

2. `ids` - collects all keys into memory at once:

```rust
let ids = DEVICES.ids()?;

for id in ids {
    // handle id
}
```

Prefer `keys` for large tables - `ids` loads all keys into memory at once and causes higher memory use.

## See also

- [How to work with state and storage](../06_state-and-storage.md#table-store) - back to the guide
- [Key-value store](./02_key-value-store.md) - many typed values under different keys
- [Time-series store](./04_time-series-store.md) - timestamped measurements

## Related SDK reference

- [Storage scopes](../../10_reference/03_myrmic-sdk/05_state-and-storage/01_storage-scopes.md)
- [Table store](../../10_reference/03_myrmic-sdk/05_state-and-storage/05_table-store.md)
