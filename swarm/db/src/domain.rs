use anyhow::Context;
pub use db_commons::models::{
    Cursor, FieldValue, Fields, NodeId, Subject, SyncPointId, Tags, Timestamp, TxId, Version,
};

pub use db_commons::models::replication::{Chunk, ScopeFrontier, Snapshot, SyncMarker, SyncMeta};

pub use crate::store::TransactionId;

pub use sem::*;
use skey::StoreKey;

mod sem;

/// This module contains the various key structures used to store data.
///
/// The keys are documented via the keys! macro in this file.
///
///  -- Storage --
/// This section deals with how things are encoded in the key-value store.
/// `BlobId`       -> `Content`
/// `Path`         -> `BlobId`
/// `BlobMeta`     -> {metadata}
/// `Measurement`  -> -empty-
/// `SemGraphName` -> {original name}
/// `TripleKey`    -> -empty-
/// `QuadKey`      -> -empty-
/// `SyncPoint`    -> `SyncMeta`

#[derive(
    Copy,
    Clone,
    Debug,
    Eq,
    PartialEq,
    PartialOrd,
    Ord,
    skey::StoreKey,
    serde::Deserialize,
    serde::Serialize,
)]
pub enum Hash {
    Sha2([u8; 32]),
}

pub type RawKey = Vec<u8>;
pub type Id = Vec<u8>;
pub type IdRef<'a> = &'a [u8];
pub type Blob = Vec<u8>;
pub type BlobRef<'a> = &'a [u8];
pub type Value = Vec<u8>;
pub type ValueRef<'a> = &'a [u8];
pub type Meta = Vec<u8>;

skey_macros::keys! {
    #[no_copy]
    use crate::semantic::EncodedTerm;

    // Scopes act as a form of tenancy and split the dataspace.
    alias scope("@", namespace: str, "*", database: str, "*", schema: str, ":");
    {
        key scope(scope);
        key table(scope, "tb", name: str, id: [u8]);
        key kv(scope, "kv", key: str);

        key measurement(scope, "ts", measurement: str, "/", timestamp: Timestamp);

        key blob_id(scope, "fs", hash: Hash);
        key blob_meta(scope, "fs", hash: Hash, "$", path: str);

        key path(scope, "fn", path: str);
    }

    /// This is a meta-structure, representing a mutation and used as a synchronisation point between nodes.
    ///
    /// It's added internally when a scope is mutated.
    /// Because it encodes the timestamp, we can use this to read a snapshot of the data at that point in time.
    key sync_point("#", scope, "sp", ts: Version, epoch: u64, id: NodeId);

    key sem_hash("sh", hash: [u8]);

    key graph_name(scope, "sg", name: EncodedTerm);

    key triple(
        scope,
        "sm",
        encoding: TriEncoding,
        a: EncodedTerm,
        b: EncodedTerm,
        c: EncodedTerm
    );

    key quad(
        scope,
        "sm",
        encoding: QuadEncoding,
        a: EncodedTerm,
        b: EncodedTerm,
        c: EncodedTerm,
        d: EncodedTerm
    );
}

pub type Scope<'a> = ScopeBuilder3<'a>;
pub type UserKey<'a> = KvBuilder4<'a>;
pub type Table<'a> = TableBuilder4<'a>;
pub type Entity<'a> = TableBuilder5<'a>;
pub type Measurement<'a> = MeasurementBuilder5<'a>;
pub type BlobId<'a> = BlobIdBuilder4<'a>;
pub type BlobMeta<'a> = BlobMetaBuilder5<'a>;
pub type Path<'a> = PathBuilder4<'a>;
pub type SyncPoint<'a> = SyncPointBuilder6<'a>;
pub type GraphName<'a> = GraphNameBuilder4<'a>;

pub type TripleKey<'a> = TripleBuilder7<'a>;
pub type QuadKey<'a> = QuadBuilder8<'a>;

impl<'a> SyncPointBuilder3<'a> {
    pub fn with_sp_id(self, id: SyncPointId) -> SyncPoint<'a> {
        self.ts(id.1).epoch(id.0).id(id.2)
    }
}

pub fn duration_since(id: SyncPointId, now: uhlc::Timestamp) -> core::time::Duration {
    let time = uhlc::NTP64(id.1);

    (now.get_time() - time).to_duration()
}

impl SyncPoint<'_> {
    pub fn range_from_subject(subject: &Subject) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
        match subject {
            Subject::Namespace(ns) => Key::sync_point().namespace(ns).range(),
            Subject::Database(ns, db) => Key::sync_point().namespace(ns).database(db).range(),
            Subject::Scope(scope) => Key::sync_point()
                .namespace(&scope.namespace)
                .database(&scope.database)
                .schema(&scope.schema)
                .range(),
        }
        .context("unable to construct sync point range")
    }

    pub fn as_id(&self) -> SyncPointId {
        (self.epoch, self.ts, self.id)
    }
}

impl Default for Scope<'_> {
    fn default() -> Self {
        // p = `public`
        // d = `default`
        // We're using short versions to reduce the needed storage.
        Key::new_scope("p", "p", "d")
    }
}

impl Scope<'_> {
    // Can't use FromStr because the input string isn't tied to the value. (aka we can't borrow from the input string...)
    pub fn parse(value: &str) -> Scope<'_> {
        let (namespace, rest) = value.split_once('/').unwrap_or((value, "p/d"));
        let (database, schema) = rest.split_once('/').unwrap_or((value, "d"));

        Key::new_scope(namespace, database, schema)
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(crate) struct MeasurementBody {
    pub(crate) tags: Tags,
    pub(crate) fields: Fields,
}

impl core::fmt::Display for Scope<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&format!(
            "{}/{}/{}",
            self.namespace, self.database, self.schema
        ))
    }
}

impl core::fmt::Display for GraphName<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&format!(
            "Graph({}/{}/{}:{:?})",
            self.namespace, self.database, self.schema, self.name
        ))
    }
}

impl core::fmt::Display for Path<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&format!(
            "Path({}/{}/{}:{})",
            self.namespace, self.database, self.schema, self.path
        ))
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ReplicationStatus {
    /// As far as this node is aware, it's caught up with all other nodes.
    Active,
    /// This node has outstanding entries it's aware of, and is attempting to get a hold of them.
    /// (aka the data that this node holds might be out of date)
    Requested,
}

pub mod api {
    pub use db_commons::models::Scope;
}
