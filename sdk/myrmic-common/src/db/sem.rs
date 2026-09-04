use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::db::Scope;

/// Wire request to run an update query against the semantic store.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct UpdateRequest {
    pub scope: Scope,
    pub query: String,
    /// Base IRI resolving relative IRIs in the query; `None` for none.
    pub base_iri: Option<String>,
}

/// Wire request to run a select query against the semantic store.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct SelectRequest {
    pub scope: Scope,
    pub query: String,
    /// Base IRI resolving relative IRIs in the query; `None` for none.
    pub base_iri: Option<String>,
    /// Maximum number of solutions to return. `None` is not unlimited: the
    /// host applies its own default page size of 100 solutions.
    pub limit: Option<usize>,
    /// Number of solutions to skip before returning any; `None` skips none.
    pub skip: Option<usize>,
}

/// Wire response to a [`SelectRequest`].
#[derive(Debug, Serialize, Deserialize)]
pub struct SelectResponse {
    /// The variable names the query selected, in column order.
    pub variables: Vec<String>,
    /// One row per solution, with a value per variable (`None` where the
    /// solution leaves a variable unbound).
    pub solutions: Vec<Vec<Option<String>>>,
}
