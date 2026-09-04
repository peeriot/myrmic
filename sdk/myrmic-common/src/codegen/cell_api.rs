//! Types representing the structure of a cell API file.
//!
//! Shared between `myrmic-build` (generates API files) and `myrmic-sdk-macros`
//! (consumes API files via the `import_cells!` macro).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct CellApi {
    pub cell: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<HashMap<String, ApiType>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub commands: HashMap<String, ApiCommand>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub events: HashMap<String, ApiEvent>,
}

/// Structural equality compares fields (name, type, and order) only, not
/// descriptions. Two `ApiType`s are equal if they are wire-compatible.
#[derive(Debug, Clone, Serialize, Deserialize, Eq)]
pub struct ApiType {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub fields: Vec<ApiField>,
}

impl ApiType {
    /// Finds a field by name, or `None` if no field with that name exists.
    pub fn find_field(&self, name: &str) -> Option<&ApiField> {
        self.fields.iter().find(|f| f.name == name)
    }
}

impl PartialEq for ApiType {
    fn eq(&self, other: &Self) -> bool {
        self.fields == other.fields
    }
}

/// Structural equality compares name and type only, not description.
/// Order matters — it is checked via the parent `Vec` comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiField {
    pub name: String,
    #[serde(default)]
    pub serde_with: Option<String>,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl PartialEq for ApiField {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.field_type == other.field_type
    }
}

impl Eq for ApiField {}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiEvent(pub ApiType);

impl AsRef<ApiType> for ApiEvent {
    fn as_ref(&self) -> &ApiType {
        &self.0
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiCommand {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub args: Option<String>,
}
