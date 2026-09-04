use core::convert::Infallible;

use alloc::string::String;
use alloc::vec::Vec;

pub use replication::ReplicaMessage;

pub mod replication;

pub type NodeId = [u8; 16];
pub type Hash32 = [u8; 32];

/// The first two u64s are an "opaque" uuid.
/// It's documented here so devs know what we're dealing with, but it's not a publicly exposed detail.
/// So if you do any uuid related shenanigans with it, it can break at any point.
///
/// The last type is the instance handling the tx itself.1
///
/// `(uuid_hi, uuid_lo, node_id)`
pub type TxId = (u64, u64, NodeId);
pub type Timestamp = u64;
pub type Version = u64;
pub type Epoch = u64;

pub type SyncPointId = (Epoch, Version, NodeId);

pub type Tags = Vec<(String, String)>;
pub type Fields = Vec<(String, FieldValue)>;

pub type Key = String;
pub type Table = String;
// Used internally to represent any key sequence. (there are internal keys that aren't utf-8)
pub type RawKey = Vec<u8>;
// @TODO (peeriot/swarm#749) jezza - 05 Feb 2026: Replace this with a 64 bit heapless vec?
pub type Id = Vec<u8>;
pub type Value = Vec<u8>;

/// Where to start a listing from.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum Cursor {
    /// start with the entry just past the given id.
    After(Id),
    /// start with the given id (inclusive).
    At(Id),
    /// start after the first N entries.
    Skip(usize),
}

#[derive(Clone, Debug, PartialOrd, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FieldValue {
    I64(i64),
    U64(u64),
    F64(f64),
    String(String),
    Boolean(bool),
}

impl core::str::FromStr for FieldValue {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // clippy be stupid...
        #![allow(clippy::same_functions_in_if_condition)]
        if let Ok(value) = s.parse() {
            Ok(Self::Boolean(value))
        } else if let Ok(value) = s.parse() {
            Ok(Self::U64(value))
        } else if let Ok(value) = s.parse() {
            Ok(Self::I64(value))
        } else if let Ok(value) = s.parse() {
            Ok(Self::F64(value))
        } else {
            Ok(Self::String(String::from(s)))
        }
    }
}

impl From<i64> for FieldValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<u64> for FieldValue {
    fn from(value: u64) -> Self {
        Self::U64(value)
    }
}

impl From<f64> for FieldValue {
    fn from(value: f64) -> Self {
        Self::F64(value)
    }
}

impl From<String> for FieldValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<bool> for FieldValue {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Scope {
    pub namespace: String,
    pub database: String,
    pub schema: String,
}

impl Scope {
    pub fn new(
        namespace: impl Into<String>,
        database: impl Into<String>,
        schema: impl Into<String>,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            database: database.into(),
            schema: schema.into(),
        }
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            // d = default
            namespace: String::from("d"),
            // d = default
            database: String::from("d"),
            // p = public
            schema: String::from("p"),
        }
    }
}

impl core::fmt::Display for Scope {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.namespace)?;
        f.write_str("/")?;
        f.write_str(&self.database)?;
        f.write_str("/")?;
        f.write_str(&self.schema)?;
        Ok(())
    }
}

/// FNV-1a over the scope's segments and the node id, NUL-separated so segment
/// boundaries can't alias.
///
/// The rendezvous draw for "which node owns `scope` among this set": argmax of
/// this hash (raw id as tie-break) is a pure function of stable, already-shared
/// state, so any node computes the same winner at any time with zero message
/// exchange — and, being scope-dependent, spreads ownership across nodes
/// instead of one id accumulating every scope. Used by custody collapse
/// (`cell-protocol`'s `custody_winner`) and by `db-client`'s scoped `any_node`
/// fallback; the two must keep agreeing on the draw.
#[must_use]
pub fn rendezvous_hash(scope: &Scope, node: &NodeId) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &byte in bytes {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    };

    eat(scope.namespace.as_bytes());
    eat(&[0]);
    eat(scope.database.as_bytes());
    eat(&[0]);
    eat(scope.schema.as_bytes());
    eat(&[0]);
    eat(node);

    hash
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum TsOrderBy {
    TimestampAsc,
    #[default]
    TimestampDesc,
}

/// Direction a table listing is returned in, ordered by entity id.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum TbOrderBy {
    #[default]
    KeyAsc,
    KeyDesc,
}

/// Selects a slice of the scope hierarchy.
/// Used to pick what replication replicates and what event subscriptions listen to.
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Subject {
    Namespace(String),
    Database(String, String),
    Scope(Scope),
}

impl Subject {
    /// Unspecified levels become keyexpr wildcards.
    pub fn as_keyexprs(&self) -> (&str, &str, &str) {
        match self {
            Self::Namespace(ns) => (&**ns, "*", "*"),
            Self::Database(ns, db) => (&**ns, &**db, "*"),
            Self::Scope(scope) => (&*scope.namespace, &*scope.database, &*scope.schema),
        }
    }

    pub fn contains(&self, scope: &Scope) -> bool {
        match self {
            Self::Namespace(ns) => &scope.namespace == ns,
            Self::Database(ns, db) => &scope.namespace == ns && &scope.database == db,
            Self::Scope(target) => scope == target,
        }
    }
}

pub mod events {
    use super::{Scope, Table, Version};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub enum TableEvent {
        /// Carries the version the commit landed at, usable as a
        /// [`tx_begin`](super::tx_begin) `min_version` to resume from it.
        Inserted(Version),
    }

    /// A decoded event, as handed to subscribers.
    #[derive(Clone, Debug)]
    pub struct Notification {
        pub scope: Scope,
        pub table: Table,
        pub event: TableEvent,
    }

    impl Notification {
        /// The version this notification's commit landed at.
        pub fn version(&self) -> Version {
            match self.event {
                TableEvent::Inserted(version) => version,
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BlobId {
    pub scope: Scope,
    pub hash: BlobHash,
}

// We could have some verification stuff on this later. (no silly characters, etc)
pub type BlobPath = String;
// We could replace this at some point, but for now, this is fine.
pub type Blob = Vec<u8>;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct BlobResponse {
    pub blob: Blob,
    /// Identifier of the blob whose data is returned in `blob`.
    ///
    /// When the request specifies `Some(ChunkRange { offset: 0, length: 0 })` for
    /// [`BlobResponse::range`], this ID refers to the full underlying blob.
    pub blob_id: BlobId,
    /// Optional range of the chunked blob
    ///
    /// The length might be clamped if the requested chunk was bigger than the available data in
    /// the blob.
    pub range: Option<ChunkRange>,
    /// Total size of the resolved blob
    pub total_len: u64,
}

/// A representation of a chunk in terms of offset + length
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChunkRange {
    /// Offset of the chunk in bytes from the start of the blob
    pub offset: u64,
    /// Length of the chunk
    pub length: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum BlobHash {
    Sha2(Hash32),
}

impl BlobHash {
    /// Computes the content hash for the given bytes.
    pub fn of(bytes: &[u8]) -> Self {
        use sha2::Digest;
        let hash: Hash32 = sha2::Sha256::digest(bytes).into();
        Self::Sha2(hash)
    }

    /// Returns the hex-encoded representation of this hash.
    pub fn to_hex(self) -> String {
        let Self::Sha2(hash) = self;
        hex::encode(hash)
    }

    /// Parses a hex-encoded hash string. Accepts an optional `0x` prefix.
    pub fn from_hex(input: &str) -> Option<Self> {
        let input = input.strip_prefix("0x").unwrap_or(input);
        let bytes = hex::decode(input).ok()?;
        let hash: Hash32 = bytes.try_into().ok()?;
        Some(Self::Sha2(hash))
    }
}

/// A request addressed to an open transaction: the transaction's id plus one
/// [`Operation`]. Every tx-scoped request module aliases this
/// (`pub type Request = super::Tx<Op>`), so there is one addressing type
/// instead of one per operation.
///
/// On the wire it is sugar: sending one resolves to a [`tx_apply`] application
/// of a single op against [`tx_apply::Target::Existing`], left open.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Tx<T> {
    pub id: TxId,
    pub op: T,
}

impl<T> Tx<T> {
    pub fn new(id: TxId, op: T) -> Self {
        Self { id, op }
    }
}

/// Reads of an operation's own fields (`req.scope`) resolve through the
/// wrapper, so addressing an op costs nothing at the call sites that read it.
impl<T> core::ops::Deref for Tx<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.op
    }
}

impl<T> core::ops::DerefMut for Tx<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.op
    }
}

/// One operation applicable inside a transaction — the canonical payload of a
/// tx-scoped request, and the unit an application batches.
pub trait Operation: Into<TxOp> + Sized {
    type Response: TryFrom<TxOpResponse>;
    type Error: From<tx_apply::Error>;

    const NAME: &'static str;

    /// Address this operation to an open transaction.
    fn at(self, id: TxId) -> Tx<Self> {
        Tx::new(id, self)
    }
}

/// An operation whose response carries nothing a caller can want, so it can be
/// buffered client-side and applied later without a round trip of its own.
///
/// The bound is what enforces the tail rule at compile time: only a
/// `Deferrable` op can be deferred, and anything returning a value must be the
/// last op of an application.
pub trait Deferrable: Operation {}

macro_rules! op_class {
    (deferrable, $module:ident) => {
        impl Deferrable for $module::Op {}
    };
    (tail, $module:ident) => {};
}

macro_rules! operations {
    ($($module:ident => $variant:ident, $class:ident, $name:literal;)*) => {
        /// Every operation an application can apply, in declaration order.
        #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
        pub enum TxOp {
            $($variant($module::Op),)*
        }

        /// One applied operation's response, mirroring [`TxOp`].
        #[derive(Debug, serde::Serialize, serde::Deserialize)]
        pub enum TxOpResponse {
            $($variant($module::Response),)*
        }

        impl TxOp {
            pub fn name(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $name,)*
                }
            }
        }

        $(
            impl From<$module::Op> for TxOp {
                fn from(op: $module::Op) -> Self {
                    Self::$variant(op)
                }
            }

            impl From<$module::Response> for TxOpResponse {
                fn from(response: $module::Response) -> Self {
                    Self::$variant(response)
                }
            }

            /// Narrowing back to the op's own response type. `Err` carries the
            /// response that arrived instead, which only a misbehaving server
            /// can produce.
            impl TryFrom<TxOpResponse> for $module::Response {
                type Error = TxOpResponse;

                fn try_from(response: TxOpResponse) -> Result<Self, Self::Error> {
                    match response {
                        TxOpResponse::$variant(response) => Ok(response),
                        other => Err(other),
                    }
                }
            }

            impl From<tx_apply::Error> for $module::Error {
                fn from(err: tx_apply::Error) -> Self {
                    Self { message: err.message }
                }
            }

            impl Operation for $module::Op {
                type Response = $module::Response;
                type Error = $module::Error;

                const NAME: &'static str = $name;
            }

            op_class!($class, $module);
        )*
    };
}

operations! {
    scope_backup => ScopeBackup, tail, "SCOPE_BACKUP";
    scope_restore => ScopeRestore, deferrable, "SCOPE_RESTORE";
    key_put => KeyPut, deferrable, "KEY_PUT";
    key_get => KeyGet, tail, "KEY_GET";
    key_delete => KeyDelete, deferrable, "KEY_DELETE";
    key_prefix => KeyPrefix, tail, "KEY_PREFIX";
    tb_insert => TbInsert, tail, "TB_INSERT";
    tb_append => TbAppend, deferrable, "TB_APPEND";
    tb_insert_batched => TbInsertBatched, tail, "TB_INSERT_BATCHED";
    tb_count => TbCount, tail, "TB_COUNT";
    tb_get => TbGet, tail, "TB_GET";
    tb_list => TbList, tail, "TB_LIST";
    tb_delete => TbDelete, deferrable, "TB_DELETE";
    ts_publish => TsPublish, deferrable, "TS_PUBLISH";
    ts_find => TsFind, tail, "TS_FIND";
    blob_store => BlobStore, tail, "BLOB_STORE";
    blob_link => BlobLink, deferrable, "BLOB_LINK";
    blob_unlink => BlobUnlink, deferrable, "BLOB_UNLINK";
    blob_move => BlobMove, deferrable, "BLOB_MOVE";
    blob_resolve => BlobResolve, tail, "BLOB_RESOLVE";
    path_resolve => PathResolve, tail, "PATH_RESOLVE";
    paths_list => PathsList, tail, "PATHS_LIST";
    sem_update => SemUpdate, deferrable, "SEM_UPDATE";
    sem_select => SemSelect, tail, "SEM_SELECT";
    sem_ask => SemAsk, tail, "SEM_ASK";
    sem_construct => SemConstruct, tail, "SEM_CONSTRUCT";
    sem_describe => SemDescribe, tail, "SEM_DESCRIBE";
}

/// The db query wire. Every write-side operation travels as a [`tx_apply`]
/// application; the rest are the requests with no transaction to apply against
/// (`Ping`, `Info`), a placement-free direct call (`TxCommit`, `TxRollback`),
/// or a read whose placement is deliberately asymmetric (`TbPeek`).
#[derive(serde::Serialize, serde::Deserialize)]
pub enum DbRequest {
    Ping(ping::Request),
    Info(db_info::Request),
    TxApply(tx_apply::Request),
    TxRollback(tx_rollback::Request),
    TxCommit(tx_commit::Request),
    TbPeek(tb_peek::Request),
}

impl DbRequest {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ping(_) => "PING",
            Self::Info(_) => "INFO",
            Self::TxApply(_) => "TX_APPLY",
            Self::TxRollback(_) => "TX_ROLLBACK",
            Self::TxCommit(_) => "TX_COMMIT",
            Self::TbPeek(_) => "TB_PEEK",
        }
    }
}

pub mod ping {
    use super::String;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Request {}

    #[derive(serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod db_info {
    use super::{NodeId, String};

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Request {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub id: NodeId,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

/// Scope discovery served by replicating nodes over a per-subject queryable
/// (see [`topics::replica_query`](crate::topics::replica_query)). The scope
/// being located is carried by the query's key expression; a node replies only
/// if it holds that scope at at least [`Request::min_version`](locate::Request::min_version).
pub mod locate {
    use super::{NodeId, Vec, Version};

    #[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
    pub struct Request {
        /// Only nodes whose head for the scope is >= this reply. `None` matches
        /// any node that holds the scope at all.
        pub min_version: Option<Version>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub id: NodeId,
        /// The node's current head version for the located scope; 0 when the
        /// node replicates the scope but holds no data for it yet.
        pub head: Version,
        /// Other live peers the responder knows hold this scope, so a single
        /// reply surfaces the whole set — a peer that was too slow to answer
        /// the query itself is still vouched for here.
        pub peers: Vec<PeerView>,
        /// How the responder holds the scope, so the client can prefer a
        /// durable replica over a node that is draining the scope away.
        pub state: HolderState,
    }

    /// One replicating peer as seen by a responder to a locate query.
    #[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
    pub struct PeerView {
        pub id: NodeId,
        /// Milliseconds since the responder last heard from this peer, in the
        /// responder's clock. Lets the client judge liveness without any
        /// cross-node clock reconciliation.
        pub age_ms: u64,
        /// The peer's last-known head version for the scope.
        pub head: Version,
        /// How the peer holds the scope, per its last announce.
        pub state: HolderState,
    }

    /// How a locate answerer holds the scope.
    ///
    /// Writes prefer a replica: a drainer still accepts a write that lands on
    /// it (availability first — and acceptance re-arms its escalation), but
    /// routing new writes past it is what lets its holdings freeze and the
    /// drain complete. Reads rank by head alone: the drainer may be the
    /// freshest, possibly only, holder of its undrained rows.
    #[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
    pub enum HolderState {
        /// A full replica — configured, or a provisional custodian.
        Replica,
        /// Serving while draining toward a durable holder.
        Draining,
    }
}

pub mod tx_begin {
    use core::time::Duration;

    use super::{Scope, String, TxId, Version};

    /// How a transaction is placed (and optionally version-bounded) when it begins.
    #[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub enum Constraint {
        /// Route the transaction to a node holding this scope, falling back to
        /// any node when none is known (e.g. the first write to a new scope).
        /// A placement hint, not a boundary: access outside the scope is not
        /// rejected.
        Routed(Scope),
        /// Like [`Routed`](Self::Routed), and additionally resume from a version
        /// observed on a table event: only a node holding the scope at *at
        /// least* this version is chosen, and that node reasserts the bound
        /// before beginning. Pairing the version with the scope keeps
        /// "a version without a scope" unrepresentable.
        RoutedAt(Scope, Version),
        // This will be a variant for specifying a subset. Will most likely be converted to a Subject form (ie namespace / namespace + database / scope)
        // This is a strict form of multiple scope (anything outside this subset will be denied)
        // Within(Vec<Scope>),
        // This is the weaker form of Within.
        // Initial(Vec<Scope>),
        #[default]
        /// All is fair in love and war.
        /// aka, no restriction.
        Ignore,
    }

    #[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
    pub struct Request {
        pub constraint: Constraint,
        pub retention_period: Option<Duration>,
        pub access: Access,
    }

    /// What the transaction is for, declared at begin.
    ///
    /// Routing keys off it: a write prefers a non-draining holder (so a
    /// drainer's holdings can freeze), a read the highest head. The fallback
    /// path does too: only a write landing on a non-replicating node makes it
    /// hold the scope — a read that located nobody leaves no trace.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub enum Access {
        Read,
        #[default]
        Write,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub id: TxId,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }

    impl Request {
        /// Begin a transaction routed to a node holding `scope`.
        pub fn routed(scope: Scope) -> Self {
            Self {
                constraint: Constraint::Routed(scope),
                ..Default::default()
            }
        }

        /// Begin a transaction routed to a node holding `scope` at at least
        /// `min_version` (e.g. a version observed on a table event).
        pub fn routed_at(scope: Scope, min_version: Version) -> Self {
            Self {
                constraint: Constraint::RoutedAt(scope, min_version),
                ..Default::default()
            }
        }
    }
}

pub mod tx_commit {
    use super::{String, TxId};

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Request {
        pub id: TxId,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod tx_rollback {
    use super::{String, TxId};

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Request {
        pub id: TxId,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod scope_backup {
    use super::{Scope, String, replication};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub snapshot: replication::Snapshot,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod scope_restore {
    use super::{Scope, String, replication};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub snapshot: replication::Snapshot,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod key_put {
    use super::{Key, Scope, String, Value};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub key: Key,
        pub value: Value,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod key_get {
    use super::{Key, Scope, String, Value};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub key: Key,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub value: Option<Value>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod key_delete {
    use super::{Key, Scope, String};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub key: Key,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod key_prefix {
    use super::{Scope, String, Vec};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub prefix: String,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub keys: Vec<String>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod tb_insert {
    use super::{Id, Scope, String, Table, Value};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub table: Table,
        pub eid: Option<Id>,
        pub value: Value,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub eid: Id,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

/// A [`tb_insert`] whose response carries nothing: the row id — server-minted
/// when `eid` is `None`, exactly as `tb_insert` mints it — is not reported
/// back, and that is what makes an insert deferrable. The typed SDK `Table`
/// surface discards ids, and so does mailbox delivery, so this is the shape
/// every hot path wants.
pub mod tb_append {
    use super::{Id, Scope, String, Table, Value};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub table: Table,
        /// `None` allocates an id server-side.
        pub eid: Option<Id>,
        pub value: Value,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod tb_insert_batched {
    use super::{Id, Scope, String, Table, Value, Vec};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub table: Table,
        /// One `(optional entity id, value)` per row. A `None` id is allocated
        /// server-side; the response returns the resolved ids in the same order.
        pub entries: Vec<(Option<Id>, Value)>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub eids: Vec<Id>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod tb_count {
    use super::{Scope, String, Table};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub table: Table,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub count: usize,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod tb_get {
    use super::{Id, Scope, String, Table, Value};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub table: Table,
        pub eid: Id,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub value: Option<Value>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod tb_list {
    use super::{Cursor, Id, Scope, String, Table, TbOrderBy, Value, Vec};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub table: Table,
        pub cursor: Option<Cursor>,
        pub limit: Option<usize>,
        /// Order entities are returned in. `None` uses the default (ascending by id).
        pub order: Option<TbOrderBy>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub entities: Vec<(Id, Value)>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod tb_delete {
    use super::{Id, Scope, String, Table};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub table: Table,
        pub eid: Id,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod ts_publish {
    use super::{Fields, Scope, String, Tags, Timestamp};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub measurement: String,
        pub tags: Tags,
        pub fields: Fields,
        pub timestamp: Timestamp,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod ts_find {
    use super::{Fields, Scope, String, Tags, Timestamp, TsOrderBy, Vec};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub measurement: String,
        pub limit: Option<usize>,
        /// Inclusive
        pub start: Option<Timestamp>,
        /// Exclusive
        pub end: Option<Timestamp>,
        /// Order of returned samples. `None` uses the default (newest-first).
        pub order: Option<TsOrderBy>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub samples: Vec<(Tags, Fields, Timestamp)>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod blob_store {
    use super::{Blob, BlobId, Scope, String};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub blob: Blob,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub blob_id: BlobId,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod blob_link {
    use super::{BlobId, BlobPath, String};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub blob_id: BlobId,
        pub path: BlobPath,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod blob_unlink {
    use super::{BlobPath, Scope, String};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub path: BlobPath,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod blob_move {
    use super::{BlobPath, Scope, String};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub old_path: BlobPath,
        pub new_path: BlobPath,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod blob_resolve {
    use super::{BlobId, BlobResponse, ChunkRange, String};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub blob_id: BlobId,
        /// Optional range of the requested chunk
        ///
        /// * *`None`*: Requests with the entire blob
        /// * *`Some(ChunkRange{ offset: _, length: 0 })`*: Can be used to just check if the blob
        ///   exists (we will be able to read the `total_len` of the response as `Some(size)`)
        /// * *`Some(ChunkRange{ offset, length })`*: Requests the requested chunk of the blob
        #[serde(default)]
        pub range: Option<ChunkRange>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub blob: Option<BlobResponse>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod path_resolve {
    use super::{BlobPath, BlobResponse, ChunkRange, Scope, String};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub path: BlobPath,
        /// Optional range of the requested chunk
        ///
        /// * *`None`*: Requests with the entire blob
        /// * *`Some(ChunkRange{ offset: _, length: 0 })`*: Can be used to just check if the blob
        ///   exists (we will be able to read the `total_len` of the response as `Some(size)`) or to
        ///   fetch the `BlobId` of the resolved path
        /// * *`Some(ChunkRange{ offset, length })`*: Requests the requested chunk of the blob
        #[serde(default)]
        pub range: Option<ChunkRange>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub blob: Option<BlobResponse>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod paths_list {
    use super::{BlobPath, Scope, String, Vec};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub limit: Option<usize>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub paths: Vec<BlobPath>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod sem_update {
    use super::{Scope, String};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub query: String,
        pub base_iri: Option<String>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod sem_select {
    use super::{Scope, String, Vec};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub query: String,
        pub base_iri: Option<String>,

        // Every part of me hates this, but it'll do for now... :(
        /// Defaults to 0
        pub skip: Option<usize>,
        /// Defaults to 100
        pub limit: Option<usize>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub variables: Vec<String>,
        pub solutions: Vec<Vec<Option<String>>>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod sem_ask {
    use super::{Scope, String};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub query: String,
        pub base_iri: Option<String>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub answer: bool,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod sem_construct {
    use super::{Scope, String, Vec};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub query: String,
        pub base_iri: Option<String>,

        // Every part of me hates this, but it'll do for now... :(
        /// Defaults to 0
        pub skip: Option<usize>,
        /// Defaults to 100
        pub limit: Option<usize>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub triples: Vec<(String, String, String)>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

pub mod sem_describe {
    use super::{Scope, String, Vec};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Op {
        pub scope: Scope,
        pub query: String,
        pub base_iri: Option<String>,

        // Every part of me hates this, but it'll do for now... :(
        /// Defaults to 0
        pub skip: Option<usize>,
        /// Defaults to 100
        pub limit: Option<usize>,
    }

    pub type Request = super::Tx<Op>;

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub triples: Vec<(String, String, String)>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

/// The write side of the db wire: one round trip that finds or continues a
/// transaction, applies a batch of operations in order, and optionally commits.
///
/// An *application*. Placement for [`tx_apply::Target::New`] is [`tx_begin`]'s contract
/// verbatim, which is the point — one placement path, whether the application
/// commits itself or leaves the transaction open for the ops still to come.
///
/// Errors are all-or-nothing. A failed [`tx_apply::Target::New`] committed nothing and
/// registered nothing, so the client may re-locate and retry; a failed
/// [`tx_apply::Target::Existing`] rolls the whole transaction back, because there are no
/// savepoints and a partially applied bundle would silently break the chain's
/// atomicity.
pub mod tx_apply {
    use core::time::Duration;

    use super::{String, TxId, TxOp, TxOpResponse, Vec, tx_begin};

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct Request {
        pub target: Target,
        /// Applied in order, atomically: all of them commit, or none.
        pub ops: Vec<TxOp>,
        pub finish: Finish,
    }

    /// The transaction an application applies against.
    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub enum Target {
        /// Place a new transaction: [`tx_begin`]'s contract, verbatim.
        New {
            constraint: tx_begin::Constraint,
            access: tx_begin::Access,
            retention_period: Option<Duration>,
        },
        /// Continue an open transaction, direct-routed via the node in its id —
        /// so no locate runs.
        Existing(TxId),
    }

    /// What becomes of the transaction once the ops have applied.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub enum Finish {
        /// Leave it open; the response carries its id.
        #[default]
        KeepOpen,
        /// Commit, publishing the table events the ops recorded.
        Commit,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        /// The open transaction, present iff the application left one open.
        pub tx: Option<TxId>,
        /// The final op's response — the tail rule on the wire. The client
        /// guarantees by construction that a value somebody needs is last.
        pub last: Option<TxOpResponse>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
        /// Which op failed, indexing [`Request::ops`]. `None` when the failure
        /// is the application's own: placement refused, transaction missing, or
        /// the commit itself.
        pub index: Option<u32>,
    }

    impl Request {
        /// One self-committing application against a newly placed transaction.
        pub fn commit_new(constraint: tx_begin::Constraint, ops: Vec<TxOp>) -> Self {
            Self {
                target: Target::New {
                    constraint,
                    access: tx_begin::Access::Write,
                    retention_period: None,
                },
                ops,
                finish: Finish::Commit,
            }
        }
    }
}

/// A routed, one-shot table read: [`tb_list`] (and optionally [`tb_count`]) in
/// a private read snapshot the server opens and closes itself, replacing a
/// begin/list/count/close sequence of separate round trips. Placement follows
/// [`tx_begin::Constraint::Routed`] read semantics; like any read, it leaves
/// no trace on the landing node.
pub mod tb_peek {
    use super::{Cursor, Id, Scope, String, Table, TbOrderBy, Value, Vec};

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Request {
        pub scope: Scope,
        pub table: Table,
        pub cursor: Option<Cursor>,
        pub limit: Option<usize>,
        /// Order entities are returned in. `None` uses the default (ascending by id).
        pub order: Option<TbOrderBy>,
        /// Also count the whole table (a full scan — see [`tb_count`](super::tb_count)).
        pub count: bool,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Response {
        pub entities: Vec<(Id, Value)>,
        /// The table's total size, when the request asked for it.
        pub count: Option<usize>,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Error {
        pub message: String,
    }
}

#[cfg(test)]
mod tests {
    use super::Subject;
    use super::{Operation, Scope, TxOp, tb_append, tx_apply, tx_begin};

    /// Pins the write-side wire. Postcard discriminants are declaration indices
    /// and its structs are bare field concatenations, so adding a [`TxOp`]
    /// variant anywhere but the end, or reordering an `Op`'s fields, silently
    /// changes what these bytes mean.
    #[test]
    fn an_application_encodes_as_pinned() {
        let request = tx_apply::Request::commit_new(
            tx_begin::Constraint::Routed(Scope::new("n", "d", "s")),
            alloc::vec![TxOp::from(tb_append::Op {
                scope: Scope::new("n", "d", "s"),
                table: alloc::string::String::from("t"),
                eid: Some(alloc::vec![7]),
                value: alloc::vec![9],
            })],
        );

        let bytes = postcard::to_allocvec(&request).expect("encodes");

        assert_eq!(
            bytes,
            alloc::vec![
                // Target::New, Constraint::Routed("n"/"d"/"s")
                0, 0, 1, b'n', 1, b'd', 1, b's', // Access::Write, retention_period: None
                1, 0, // one op: TxOp::TbAppend
                1, 7, // scope "n"/"d"/"s", table "t"
                1, b'n', 1, b'd', 1, b's', 1, b't', // eid Some([7]), value [9]
                1, 1, 7, 1, 9, // Finish::Commit
                1,
            ],
        );
    }

    /// Addressing an op is a nested struct, which postcard concatenates — so it
    /// costs nothing over spelling the transaction id and the payload flat.
    #[test]
    fn addressing_an_op_costs_no_bytes() {
        let op = tb_append::Op {
            scope: Scope::new("n", "d", "s"),
            table: alloc::string::String::from("t"),
            eid: None,
            value: alloc::vec![9],
        };

        let id = (1u64, 2u64, [3u8; 16]);
        let addressed = postcard::to_allocvec(&op.clone().at(id)).expect("encodes");

        let mut flat = postcard::to_allocvec(&id).expect("encodes");
        flat.extend(postcard::to_allocvec(&op).expect("encodes"));

        assert_eq!(addressed, flat);
    }

    #[test]
    fn subject_wildcards_unspecified_levels() {
        let ns = Subject::Namespace("n".into());
        assert_eq!(ns.as_keyexprs(), ("n", "*", "*"));

        let db = Subject::Database("n".into(), "d".into());
        assert_eq!(db.as_keyexprs(), ("n", "d", "*"));

        let scope = Subject::Scope(super::Scope::new("n", "d", "s"));
        assert_eq!(scope.as_keyexprs(), ("n", "d", "s"));
    }
}
