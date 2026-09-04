use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::db::Scope;

/// Wire request to store a value under a key, replacing any existing value.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct PutRequest {
    pub scope: Scope,
    pub key: String,
    pub value: Vec<u8>,
}

/// Wire request to fetch the value stored under a key.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct GetRequest {
    pub scope: Scope,
    pub key: String,
}

/// Wire response to a [`GetRequest`].
#[derive(Debug, Serialize, Deserialize)]
pub struct GetResponse {
    /// The stored value, or `None` if the key is absent.
    pub payload: Option<Vec<u8>>,
}

/// Wire request to delete the value stored under a key, if any.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct DeleteRequest {
    pub scope: Scope,
    pub key: String,
}

/// Wire request to list the full keys stored under a prefix.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct PrefixRequest {
    pub scope: Scope,
    pub prefix: String,
}

/// Wire response to a [`PrefixRequest`].
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct PrefixResponse {
    pub keys: Vec<String>,
}
