# State

`State<T>` is the SDK abstraction for storing one typed value under one fixed key. This page explains each operation in detail.

## Declare

State is declared as a handle bound to a key and a type. There are two ways to create one:

1. As a module-level constant at compile time - when the key is a fixed string known when writing the code:

```rust
use myrmic_sdk::db::state::State;

const THRESHOLD: State<f32> = State::new_const("threshold");
```

2. At runtime-from within a handler-when the key is dynamic and depends on information available only while the program is running:

```rust
let state = State::<f32>::new(&device_id);
```

Both bind to the default scope - private to the cell. To use a custom scope, pass it explicitly using `new_const_in` or `new_in`:

```rust
use myrmic_sdk::db::Scope;

// Compile time - private with a custom schema
const THRESHOLD: State<f32> = State::new_const_in(
    "threshold",
    Scope::private_in(Some("config")),
);

// Runtime - public, shared across cells using the same scope
let state = State::<f32>::new_in(
    &device_id,
    Scope::public("my-app"),
);
```

## Write

The state handle exposes a `save` method. It takes a reference to the value and writes it to the runtime database under the handle's key, overwriting any existing value.

```rust
THRESHOLD.save(&30.0)?;
```

To write under a different key without declaring a separate handle, use `save_to`. It takes the key and a reference to the value:

```rust
THRESHOLD.save_to("threshold-zone-a", &30.0)?;
```

## Read

The state handle exposes a `load` method. It reads the value from the runtime database and returns it, or `None` if no value has been stored yet - provide a fallback to handle that case.

```rust
let value = THRESHOLD.load()?.unwrap_or(25.0);
```

To read from a different key without declaring a separate handle, use `load_from`. It takes the key and returns the value the same way:

```rust
let value = THRESHOLD.load_from("threshold-zone-a")?.unwrap_or(25.0);
```

## Modify

Modifying a stored value is a read-change-write cycle. The state handle provides several methods for this, grouped by how the mutation is expressed.

### Closure

A closure-based approach passes the current value by mutable reference, runs the closure on it, and saves the result back.

The state handle exposes a `modify` method. It applies the closure to the stored value, saves the result, and returns the updated value - or `None` if nothing was stored:

```rust
let updated = THRESHOLD.modify(|value| {
    *value += 1.0;
})?;
```

To apply the closure on a different key, use `modify_at`:

```rust
let updated = THRESHOLD.modify_at("threshold-zone-a", |value| {
    *value += 1.0;
})?;
```

### Guard

A guard follows Rust's RAII pattern - it loads the value, lets you mutate it directly, and saves it back automatically when it is dropped.

The state handle exposes a `guard` method. It returns a guard only when a value is already stored - `None` if nothing is there:

```rust
if let Some(mut guard) = THRESHOLD.guard()? {
    *guard += 1.0;
}
```

To open a guard on a different key, use `guard_at`:

```rust
if let Some(mut guard) = THRESHOLD.guard_at("threshold-zone-a")? {
    *guard += 1.0;
}
```

To always get a guard - even when nothing is stored yet - use `guard_or_default`. It falls back to the type's default when nothing is stored and then yields the guard:

```rust
let mut guard = THRESHOLD.guard_or_default()?;
*guard += 1.0;
```

To do the same on a different key, use `guard_or_default_at`:

```rust
let mut guard = THRESHOLD.guard_or_default_at("threshold-zone-a")?;
*guard += 1.0;
```

### Upsert

Upsert ensures a value is always present - it returns what is stored, or saves the type's default and returns it if nothing is there.

The state handle exposes an `upsert` method:

```rust
let value = THRESHOLD.upsert()?;
```

To do the same on a different key, use `upsert_at`:

```rust
let value = THRESHOLD.upsert_at("threshold-zone-a")?;
```

To apply a closure before saving - starting from the type's default when nothing is stored - use `upsert_with`. It always runs the closure and returns the updated value:

```rust
let value = THRESHOLD.upsert_with(|value| {
    *value += 1.0;
})?;
```

To do the same on a different key, use `upsert_with_at`:

```rust
let value = THRESHOLD.upsert_with_at("threshold-zone-a", |value| {
    *value += 1.0;
})?;
```

## See also

- [How to work with state and storage](../06_state-and-storage.md#state) - back to the guide
- [Key-value store](./02_key-value-store.md) - many typed values under different keys

## Related SDK reference

- [Storage scopes](../../10_reference/03_myrmic-sdk/05_state-and-storage/01_storage-scopes.md)
- [Transient state](../../10_reference/03_myrmic-sdk/05_state-and-storage/02_transient-state.md)
- [Persistent cell state](../../10_reference/03_myrmic-sdk/05_state-and-storage/03_persistent-state.md)
