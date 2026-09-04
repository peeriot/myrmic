use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::db::Scope;

/// Where to start a table listing from.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Cursor {
    /// start with the entry just past the given id.
    After(Vec<u8>),
    /// start with the given id (inclusive).
    At(Vec<u8>),
    /// start after the first N entries.
    Skip(usize),
}

/// Direction a table listing is returned in, ordered by entity id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[allow(missing_docs)]
pub enum TbOrderBy {
    #[default]
    KeyAsc,
    KeyDesc,
}

/// A [`TbInsertRequest`] that reports no id back. Costs no round trip of its
/// own: the host buffers it into the handler's transaction.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct TbAppendRequest {
    pub scope: Scope,
    pub table: String,
    /// The entity id to store the row under; `None` lets the host generate
    /// one.
    pub eid: Option<Vec<u8>>,
    pub value: Vec<u8>,
}

/// Wire request to insert a row into a table.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct TbInsertRequest {
    pub scope: Scope,
    pub table: String,
    /// The entity id to store the row under; `None` lets the host generate
    /// one.
    pub eid: Option<Vec<u8>>,
    pub value: Vec<u8>,
}

/// Wire response to a [`TbInsertRequest`].
#[derive(Debug, Serialize, Deserialize)]
pub struct TbInsertResponse {
    /// The entity id the row was stored under.
    pub eid: Vec<u8>,
}

/// Wire request for the number of rows in a table.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct TbCountRequest {
    pub scope: Scope,
    pub table: String,
}

/// Wire response to a [`TbCountRequest`].
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct TbCountResponse {
    pub count: usize,
}

/// Wire request to fetch one row from a table by entity id.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct TbGetRequest {
    pub scope: Scope,
    pub table: String,
    pub eid: Vec<u8>,
}

/// Wire response to a [`TbGetRequest`].
#[derive(Debug, Serialize, Deserialize)]
pub struct TbGetResponse {
    /// The encoded row value, or `None` if there is no such row.
    pub value: Option<Vec<u8>>,
}

/// Wire request to list rows from a table.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct TbListRequest {
    pub scope: Scope,
    pub table: String,
    /// Where to start listing from; `None` starts at the beginning.
    pub cursor: Option<Cursor>,
    /// Maximum number of rows to return; `None` for no limit.
    pub limit: Option<usize>,
    /// Order entities are returned in. `None` uses the default (ascending by id).
    pub order: Option<TbOrderBy>,
}

/// Wire response to a [`TbListRequest`].
#[derive(Debug, Serialize, Deserialize)]
pub struct TbListResponse {
    /// The listed `(entity id, encoded value)` rows.
    pub entities: Vec<(Vec<u8>, Vec<u8>)>,
}

/// Wire request to delete one row from a table by entity id, if present.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct TbDeleteRequest {
    pub scope: Scope,
    pub table: String,
    pub eid: Vec<u8>,
}
