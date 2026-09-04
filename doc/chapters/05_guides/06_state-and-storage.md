# State and storage

During its lifetime, a cell reads, creates, and modifies data. To choose the right way to store that data, it is important to understand how Myrmic works.

Mymric cells run as Wasm modules. A Wasm module has its own linear memory, but this memory is temporary. It is initialized whenever a handler runs, such as a command, event, or scheduled handler, and it is discarded when the handler finishes.

This is a fundamental property of the Wasm execution model: a cell cannot hold data in its own memory across calls.

To preserve data between handler invocations, a cell stores it in the database provided by the Myrmic runtime. The database provides persistent and scoped storage, replicates the stored data, and supports retention policies.

The runtime database supports several storage models: key-value, table-row, time series, and RDF. The Myrmic SDK provides the tools needed to work with each model.

All storage operations take place within a scope. Before looking at the available storage modes, we will explain how scopes organize data and control access.

## Scope

All storage models use scopes, either explicitly or implicitly. A scope organizes data, controls who can access it, and establishes tenancy boundaries. Every piece of data belongs to a scope, and every storage operation takes place within one.

A scope is structured as `{namespace}/{database}/{schema}`.

There are two scope types:

- **Private** - data under this scope is bound to the cell instance. Other cells cannot read or write it.
- **Public** - data under this scope is shared and accessible across all cells using the same scope definition.

```rust
use myrmic_sdk::db::Scope;

// Private - data is bound to this cell instance
let scope = Scope::private();

// Private with a custom schema
let scope = Scope::private_in(Some("my-schema"));

// Public - data is shared across all cells using the same scope
let scope = Scope::public("my-namespace");

// Public with custom database and schema
let scope = Scope::public_in("my-namespace", Some("my-database"), Some("my-schema"));

// Default - equivalent to Scope::private()
let scope = Scope::default();
```

## Storage Modes

### State

A cell often needs to keep a value between handler invocations, such as a counter, threshold, or flag.

The Myrmic SDK provides `State<T>` - a simplified interface to the runtime's key-value database. A state handle associates one fixed key with one typed value, which can be a scalar or a structured object. The value is read and written as a complete unit and can be accessed from any handler in the cell.

`State<T>` provides the following operations:

- [Declare](./06_state-and-storage/01_state.md#declare) - Create a typed state handle and bind it to a key and scope
- [Write](./06_state-and-storage/01_state.md#write) - Save a value under the handle's key, replacing any value already stored there
- [Read](./06_state-and-storage/01_state.md#read) - Load the stored value, or receive none if no value exists
- [Modify](./06_state-and-storage/01_state.md#modify) - Load, change, and save a value in a single operation
- [Guard](./06_state-and-storage/01_state.md#guard) - Access a value through a mutable guard that saves it automatically when dropped
- [Upsert](./06_state-and-storage/01_state.md#upsert) - Return the stored value, or create and return a default value if none exists

Let us see [how to work with state in practice](./06_state-and-storage/01_state.md).

### Key-value Store

The key-value store manages many typed values under different keys. The Myrmic SDK provides `Kv<V>` as a typed key-value store handle rooted at a prefix.

`Kv<V>` provides the following operations:

- [Declare](./06_state-and-storage/02_key-value-store.md#declare) - Create a typed key-value handle and bind it to a prefix and scope
- [Write](./06_state-and-storage/02_key-value-store.md#write) - Store a value under a key, replacing any value already stored there
- [Read](./06_state-and-storage/02_key-value-store.md#read) - Retrieve the value stored under a key, or receive none if the key does not exist
- [Delete](./06_state-and-storage/02_key-value-store.md#delete) - Remove the value stored under a key
- [Iterate](./06_state-and-storage/02_key-value-store.md#iterate) - Process entries under the full prefix or a selected sub-prefix
- [Keys](./06_state-and-storage/02_key-value-store.md#keys) - Retrieve stored keys without loading their values

#### State and key-value comparison

`State<T>` and `Kv<T>` both store typed values in the runtime's key-value database. Each value can be a struct and is read or written as a complete unit. The difference is cardinality:

- `State<T>` manages one value under one fixed key.
- `Kv<T>` manages many values under different keys beneath one prefix.

Both interfaces can store the same type:

```rust
use myrmic_sdk::db::state::State;
use myrmic_sdk::db::tree::Kv;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct SensorConfig {
    threshold: f32,
    enabled: bool,
}
```

Use `State<T>` when the cell needs one sensor configuration:

```rust
const CONFIG: State<SensorConfig> = State::new_const("config");

let config = SensorConfig {
    threshold: 30.0,
    enabled: true,
};

CONFIG.save(&config)?;
let config = CONFIG.load()?;
```

This creates one entry:

```text
config -> SensorConfig
```

Use `Kv<T>` when the cell needs configurations for multiple sensors:

```rust
const CONFIGS: Kv<SensorConfig> = Kv::new("configs");

let sensor_01 = SensorConfig {
    threshold: 25.0,
    enabled: true,
};

let sensor_02 = SensorConfig {
    threshold: 30.0,
    enabled: false,
};

CONFIGS.put("sensor-01", &sensor_01)?;
CONFIGS.put("sensor-02", &sensor_02)?;

let config = CONFIGS.get("sensor-01")?;
```

This creates multiple entries under the same prefix:

```text
configs/sensor-01 -> SensorConfig
configs/sensor-02 -> SensorConfig
```

In both cases, `SensorConfig` is typed and read or written as a complete value. The difference is one fixed key per `State` handle versus many keys under a `Kv` prefix.

Let us see [how to work with the key-value store in practice](./06_state-and-storage/02_key-value-store.md).

### Table Store

The runtime database supports a table model - a named collection of typed entries, each identified by a key. Entries can be inserted with an auto-assigned UUID or an explicit key, looked up by ID, deleted, counted, and iterated in order.

The Myrmic SDK provides `Table<V>` as the abstraction for this.

`Table<V>` provides the following operations:

- [Declare](./06_state-and-storage/03_table-store.md#declare) - Create a typed table handle and bind it to a table name and scope
- [Insert](./06_state-and-storage/03_table-store.md#insert) - Store an entry under an automatically assigned UUID or an explicit key
- [Get by ID](./06_state-and-storage/03_table-store.md#get-by-id) - Retrieve an entry by its key, or receive none if it does not exist
- [Delete](./06_state-and-storage/03_table-store.md#delete) - Remove an entry by its key
- [Count](./06_state-and-storage/03_table-store.md#count) - Return the number of entries in the table
- [Iterate](./06_state-and-storage/03_table-store.md#iterate) - Process table entries in ascending or descending key order
- [Keys](./06_state-and-storage/03_table-store.md#keys) - Iterate over the keys or load all IDs into memory

Let us see [how to work with the table store in practice](./06_state-and-storage/03_table-store.md).

### Time-series Store

The runtime database supports a time-series model, to store timestamped records - multiple records can belong to the same subject, each capturing a point in time.

Each entry is called a **measurement** and is made up of:

- a **name** that groups related measurements into a series
- **fields** - a list of named, typed values carrying the data
- **tags** - optional string metadata attached to the entry
- a **timestamp**

Each write is additive - it adds a new measurement without affecting previous ones. Measurements are permanent and cannot be deleted.

Unlike the other storage models, the SDK does not provide an abstraction layer for the time-series store - all interactions happen directly through functions.

The time-series store provides the following operations:

- [Write](./06_state-and-storage/04_time-series-store.md#write) - Add a timestamped measurement without changing earlier measurements
- [Query](./06_state-and-storage/04_time-series-store.md#query) - Retrieve measurements by name, with optional time-range, limit, and ordering parameters

Let us see [how to work with the time-series store in practice](./06_state-and-storage/04_time-series-store.md).

### Semantic Store

The runtime database also supports a semantic store - an [RDF](https://www.w3.org/TR/rdf11-concepts/) triple store where data is written and queried with [SPARQL](https://www.w3.org/TR/sparql11-query/).

Like the time-series store, the SDK does not provide an abstraction layer - all interactions happen directly through functions.

The semantic store provides the following operations:

- [Update](./06_state-and-storage/05_semantic-store.md#update) - Insert, modify, or delete RDF triples by executing a SPARQL update
- [Select](./06_state-and-storage/05_semantic-store.md#select) - Query RDF data by executing a SPARQL `SELECT` query

Let us see [how to work with the semantic store in practice](./06_state-and-storage/05_semantic-store.md).

## Buffers and serialization

Working with the time-series and semantic stores feels different from State, Kv, and Table - there are extra buffers to manage in the picture.

The reason is that all storage operations cross a boundary between the Wasm cell and the host runtime - and that communication happens through byte buffers. The abstraction the SDK provides for state, key-value, and table storage hides that complexity and manages the buffers for you. That is also why every type stored through them must derive `Serialize` and `Deserialize` from [serde](https://serde.rs), since under the hood encoding and decoding happens at every read and write.

Two things to keep in mind:

1. For state, key-value, and table storage - types used as values must derive `Serialize` and `Deserialize`.
2. For the time-series and semantic stores - make sure your buffers are large enough to hold your request and response.

## Exporting and importing data

Data stored in the runtime database is persistent - but there are scenarios where you need to move it: taking a snapshot before a migration, restoring after an incident, or seeding a new environment with existing data.

The Myrmic CLI provides two commands for this:

- `myrmic database export` - snapshots a database scope to a destination
- `myrmic database import` - restores a database scope from a source

Let us look at some examples of use:

1. Export to a local file:

```bash
myrmic database export my-app/my-database/my-schema --target /var/backups/snapshot
```

2. Export to S3:

```bash
myrmic database export my-app/my-database/my-schema --target s3://my-bucket/my-snapshot --region eu-west-1
```

3. Restore from a file:

```bash
myrmic database import --source /var/backups/snapshot
```

For the full synopsis, explanation, and more examples, see [`myrmic database export`](../10_reference/02_myrmic-cli/13_database/01_export.md) and [`myrmic database import`](../10_reference/02_myrmic-cli/13_database/02_import.md).

## See also

- [Cells](./01_cells.md) - introduction to the cell model
- [Myrmic CLI reference](../10_reference/02_myrmic-cli.md)

## Related SDK reference

- [Storage scopes](../10_reference/03_myrmic-sdk/05_state-and-storage/01_storage-scopes.md)
- [Transient state](../10_reference/03_myrmic-sdk/05_state-and-storage/02_transient-state.md)
- [Persistent cell state](../10_reference/03_myrmic-sdk/05_state-and-storage/03_persistent-state.md)
- [Key-value store](../10_reference/03_myrmic-sdk/05_state-and-storage/04_key-value-store.md)
- [Table store](../10_reference/03_myrmic-sdk/05_state-and-storage/05_table-store.md)
- [Time-series store](../10_reference/03_myrmic-sdk/05_state-and-storage/06_time-series-store.md)
- [Semantic store](../10_reference/03_myrmic-sdk/05_state-and-storage/07_semantic-store.md)
- [Blob store](../10_reference/03_myrmic-sdk/05_state-and-storage/08_blob-store.md)
