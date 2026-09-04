# Storage scopes

> **Availability:** Linux and embedded runtimes

Every operation on the runtime database takes place within a scope.

A scope decides where data belongs and who can reach it, and it guarantees tenancy.

A scope is either private, holding data bound to a cell alone, or public, holding data shared by every cell that uses it.

A scope is written `namespace/database/schema`.

## Operations

- Declare a private scope, and optionally its schema.
- Declare a public scope, and optionally its database and schema.
- Pass a scope to any storage operation.

## Example

```rust
use myrmic_sdk::db::state::State;
use myrmic_sdk::db::Scope;
use myrmic_sdk::Metadata;

// Private: bound to this cell. Only the schema can be declared, because the
// runtime uses the rest for the cell's identity.
const OWN: Scope = Scope::private_in(Some("readings"));

// Public: shared by every cell using it. Both parts can be declared.
const SHARED: Scope = Scope::public_in("application-data", Some("metrics"), Some("v1"));

const LOCAL: State<u64> = State::new_const_in("count", OWN);
const TOTAL: State<u64> = State::new_const_in("count", SHARED);

#[myrmic_sdk::cmd]
fn record(_md: Metadata) -> myrmic_sdk::Result {
    // The same key in two scopes is two separate values.
    LOCAL.upsert_with(|count| *count += 1)?;
    TOTAL.upsert_with(|count| *count += 1)?;

    Ok(())
}
```

## Behavior

### Normal

The runtime resolves a private scope from the cell's own SRI, so no cell can reach another cell's. It fixes the namespace and the database, which is why only the schema is left to declare.

### Errors

A declared part of a scope may not be empty.

A cell may not use a namespace the system keeps for itself.

### Limits

A public scope separates data; it does not protect it. Two cells using the same one overwrite each other freely.

## API documentation

For every constructor, see [`Scope`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/struct.Scope.html).
