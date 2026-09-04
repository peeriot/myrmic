//! The ID types are used to identify entities within the system. Note that the scope in which different ID types are unique
//! differs depending on the type. See the documentation of the individual type for more details.
//!
//! Generally, the system guarantees that the IDs are unique within their individual scopes, meaning that if an entity with
//! a given ID is not found, it does not exist in the system.

use std::fmt::Display;

use crate::{Error, bail_validation};
use serde::{Deserialize, Serialize, de};
use uuid::Uuid;
use zenoh::key_expr::keyexpr;

/// The ID identifying a task within an application.
/// Unique within that specific application.
#[derive(Debug, Serialize, Hash, Clone, Eq)]
pub struct TaskId(String);

impl<T> PartialEq<T> for TaskId
where
    T: AsRef<str>,
{
    fn eq(&self, other: &T) -> bool {
        self.0 == other.as_ref()
    }
}

impl AsRef<str> for TaskId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for TaskId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        TaskId::try_from(s).map_err(de::Error::custom)
    }
}

impl TryFrom<String> for TaskId {
    type Error = Error;

    fn try_from(string: String) -> Result<Self, Self::Error> {
        if usable_within_key_expressions(&string) {
            Ok(Self(string))
        } else {
            let err_msg = format!(
                "The provided task id '{string}' is not valid. task IDs must be usable in key expressions and must not contain any of the characters '/$#?'."
            );
            Err(Error::validation(err_msg))
        }
    }
}

impl Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{id}", id = self.0)
    }
}

/// Identifier for both inputs and outputs
/// - The inputs are unique on the task level
/// - The outputs are unique on the task level
#[derive(Debug, Serialize, Hash, Clone, PartialEq, Eq)]
pub struct PortId(String);

impl TryFrom<String> for PortId {
    type Error = Error;

    fn try_from(string: String) -> Result<Self, Self::Error> {
        if usable_within_key_expressions(&string) {
            Ok(Self(string))
        } else {
            bail_validation!(
                "The provided port id '{string}' is not valid. Port IDs must be usable in key expressions and must not contain any of the characters '/$#?'."
            );
        }
    }
}

impl<'de> Deserialize<'de> for PortId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        PortId::try_from(s).map_err(de::Error::custom)
    }
}

impl AsRef<str> for PortId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Display for PortId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{id}", id = self.0)
    }
}

/// Unique identifier for a deployed application
/// Note that the same application may be deployed multiple times, in which case each deployment
/// is deployed with a unique ID.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Copy, Hash)]
pub struct DeploymentId {
    id: Uuid,
}

impl Default for DeploymentId {
    fn default() -> Self {
        let id = Uuid::new_v4();

        // we are using the depl id as part of some topics. This should in general be chill with any ID,
        // but it does not hurt to double check while in debug mode
        debug_assert!(usable_within_key_expressions(id.to_string()));
        Self { id }
    }
}

impl From<Uuid> for DeploymentId {
    fn from(id: Uuid) -> Self {
        debug_assert!(usable_within_key_expressions(id.to_string()));
        Self { id }
    }
}

impl DeploymentId {
    #[must_use]
    pub fn starts_with(&self, prefix: impl AsRef<str>) -> bool {
        let id_string = self.id.to_string();
        id_string.starts_with(prefix.as_ref())
    }
}

impl Display for DeploymentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{id}", id = self.id)
    }
}

fn usable_within_key_expressions(string: impl AsRef<str>) -> bool {
    let string = string.as_ref();
    if string.contains('/') {
        return false;
    }

    let key_ex = keyexpr::new(string);
    key_ex.is_ok()
}

#[cfg(test)]
mod test {
    use crate::reference::ids::usable_within_key_expressions;

    #[test]
    fn string_rejected_when_not_usable_within_kes() {
        assert!(usable_within_key_expressions("valid"), "valid");
        assert!(
            usable_within_key_expressions("valid with space"),
            "valid with space"
        );
        assert!(
            usable_within_key_expressions("valid-with-hyphens"),
            "valid with hyphens"
        );

        assert!(
            !usable_within_key_expressions("invalid/"),
            "invalid trailing slash"
        );
        assert!(
            !usable_within_key_expressions("/invalid"),
            "invalid starting slash"
        );
        assert!(
            !usable_within_key_expressions("in//valid"),
            "invalid inner double slash"
        );
        assert!(
            !usable_within_key_expressions("in/valid"),
            "invalid inner slash"
        );
        assert!(
            !usable_within_key_expressions("in?valid"),
            "invalid question mark"
        );
        assert!(!usable_within_key_expressions("in#valid"), "invalid hash");
        assert!(
            !usable_within_key_expressions("in$valid"),
            "invalid dollar sign"
        );
    }
}
