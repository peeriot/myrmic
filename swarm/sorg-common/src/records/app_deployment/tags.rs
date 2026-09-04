//! Capability tag requirements used to constrain where a cell may be placed.

use serde::{Deserialize, Serialize};

/// Represents the requirements of the corresponding task (a runtime/node must fulfill all requirements
/// to be considered as placement target).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct RequirementTags(Vec<RequirementTag>);

impl RequirementTags {
    pub fn new<T: Into<String>>(tags: Vec<T>) -> Self {
        Self(tags.into_iter().map(RequirementTag::new).collect())
    }
}

impl AsRef<[RequirementTag]> for RequirementTags {
    fn as_ref(&self) -> &[RequirementTag] {
        &self.0
    }
}

/// Represents a requirement of the corresponding task.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RequirementTag(String);

impl AsRef<str> for RequirementTag {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl RequirementTag {
    pub(crate) fn new(tag_str: impl Into<String>) -> Self {
        Self(tag_str.into())
    }
}
