//! Datalayer access: the raw key-value, table, time-series, semantic, and blob
//! APIs, plus the typed [`state`], [`store`], [`table`], and [`tree`] wrappers
//! over them.
//!
//! # Choosing a store
//!
//! | Handle | Keys | Best for |
//! |---|---|---|
//! | [`State`](state::State) | one fixed key | a single value read and updated as a whole |
//! | [`Kv`](tree::Kv) | UTF-8 strings, `/`-hierarchical | data enumerated one subtree at a time (cheap prefix scans) |
//! | [`Table`](table::Table) | raw entity-id bytes ([`Sri`](crate::Sri), `Vec<u8>`, `String`) | typed rows with ordered iteration, counting, and host-generated ids |
//! | [`BlobStore`](store::BlobStore) | paths | files and static assets (content-addressed, served by the [`gateway`](crate::gateway)) |
//! | [`publish_measurement`] / [`find_measurement`] | series name + tags | time-series samples |
//! | [`sem_update`] / [`sem_select`] | graph queries | semantic (RDF) data |
//!
//! Everything here is durable — private db state survives restarts and
//! respawns. State that must *not* be persisted (host resource handles)
//! belongs in [`InMemory`](crate::InMemory) instead.

pub mod blob;
mod kv;
mod sem;
mod tb;
mod ts;

#[cfg(feature = "cells")]
pub mod state;
#[cfg(feature = "cells")]
pub mod store;
#[cfg(feature = "cells")]
pub mod table;
#[cfg(feature = "cells")]
pub mod tree;

pub use kv::{key_delete, key_get, key_prefix, key_put};
pub use sem::{sem_select, sem_update};
pub use tb::{tb_append, tb_count, tb_delete, tb_get, tb_insert, tb_list};
pub use ts::{find_measurement, publish_measurement};

// re-export the stuff from `myrmic-common` that we need when writing module logic
pub use myrmic_common::db::{
    BlobId, BlobPath, BlobResponse, ChunkRange, Cursor, FieldValue, FindResponse, Measurement,
    Namespace, PublishRequest, Sample, Scope, SelectResponse, TbOrderBy, TsOrderBy,
};

// for now, we are working with constant-size buffers that we allocate on the stack
// we could later extend the api to allow the user to specify the maximal size of buffers
const MAX_SIZE_COMM_BUFFER: usize = 100;
