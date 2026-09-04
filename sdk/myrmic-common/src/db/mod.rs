//! Wire types for the datalayer APIs: the key-value, table, time-series,
//! semantic, and blob stores, plus the [`Scope`] every operation addresses.

use alloc::borrow::Cow;
use alloc::string::String;
use serde::{Deserialize, Serialize};

pub use blob::{
    BlobHash, BlobId, BlobLinkRequest, BlobMoveRequest, BlobPath, BlobResolveRequest, BlobResponse,
    BlobStoreRequest, BlobStoreResponse, BlobUnlinkRequest, ChunkRange, PathResolveRequest,
    PathsListRequest, PathsListResponse, ResolveResponse,
};
pub use kv::{DeleteRequest, GetRequest, GetResponse, PrefixRequest, PrefixResponse, PutRequest};
pub use sem::{SelectRequest, SelectResponse, UpdateRequest};
pub use tb::{
    Cursor, TbAppendRequest, TbCountRequest, TbCountResponse, TbDeleteRequest, TbGetRequest,
    TbGetResponse, TbInsertRequest, TbInsertResponse, TbListRequest, TbListResponse, TbOrderBy,
};
pub use ts::{
    FieldValue, FindRequest, FindResponse, Measurement, PublishRequest, Sample, TsOrderBy,
};

mod blob;
mod kv;
mod sem;
mod tb;
mod ts;

/// Where a db operation reads and writes.
///
/// The namespace decides whose data it is: [`Namespace::Private`] is the
/// calling cell's own slice — the host pins the namespace and database to the
/// cell's identity, and only `schema` narrows it further. A public namespace
/// is shared by every cell naming it, narrowed by `database` and `schema`.
/// Anything left unset falls back to a host default.
///
/// The constructors come in two flavours: the `const` ones take string literals,
/// so a cell can declare a scope next to the table that uses it, and the
/// `*_owned` ones take strings computed at runtime.
///
/// ```ignore
/// const CHAT: Scope = Scope::public("chatty");
/// pub const USERS: Table<User, Sri> = Table::new_in("users", CHAT);
///
/// let per_cell = Scope::public_owned(ns, None, Some(sri.to_string()));
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scope {
    namespace: Namespace,
    database: Option<Cow<'static, str>>,
    schema: Option<Cow<'static, str>>,
}

impl Default for Scope {
    fn default() -> Self {
        Self::private()
    }
}

impl Scope {
    /// The calling cell's own slice, with the host default schema.
    pub const fn private() -> Self {
        Self::private_in(None)
    }

    /// The shared `namespace`, with host default database and schema.
    pub const fn public(namespace: &'static str) -> Self {
        Self::public_in(namespace, None, None)
    }

    /// The calling cell's own slice, narrowed to `schema`.
    pub const fn private_in(schema: Option<&'static str>) -> Self {
        Self {
            namespace: Namespace::Private,
            database: None,
            schema: borrowed(schema),
        }
    }

    /// The shared `namespace`, narrowed to `database` / `schema`.
    pub const fn public_in(
        namespace: &'static str,
        database: Option<&'static str>,
        schema: Option<&'static str>,
    ) -> Self {
        Self {
            namespace: Namespace::Public(Cow::Borrowed(namespace)),
            database: borrowed(database),
            schema: borrowed(schema),
        }
    }

    /// [`Scope::private_in`] over a string built at runtime.
    #[must_use]
    pub fn private_owned(schema: Option<String>) -> Self {
        Self {
            namespace: Namespace::Private,
            database: None,
            schema: schema.map(Cow::Owned),
        }
    }

    /// [`Scope::public_in`] over strings built at runtime.
    #[must_use]
    pub fn public_owned(
        namespace: String,
        database: Option<String>,
        schema: Option<String>,
    ) -> Self {
        Self {
            namespace: Namespace::Public(Cow::Owned(namespace)),
            database: database.map(Cow::Owned),
            schema: schema.map(Cow::Owned),
        }
    }

    /// Decomposes the scope into its `(namespace, database, schema)` parts.
    #[must_use]
    pub fn into_inner(
        self,
    ) -> (
        Namespace,
        Option<Cow<'static, str>>,
        Option<Cow<'static, str>>,
    ) {
        (self.namespace, self.database, self.schema)
    }
}

/// `Option::map` is not const, so the const constructors go through this.
const fn borrowed(s: Option<&'static str>) -> Option<Cow<'static, str>> {
    match s {
        Some(s) => Some(Cow::Borrowed(s)),
        None => None,
    }
}

/// Whose data a [`Scope`] addresses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Namespace {
    /// The calling cell's own slice; the host pins it to the cell's identity,
    /// so no other cell can name it.
    Private,
    /// A shared namespace, addressable by every cell naming it.
    Public(Cow<'static, str>),
}
