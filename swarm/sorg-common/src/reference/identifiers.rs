//! The identifier types are used to conveniently refer to entities in the system. In contrast to the ID types, referring to
//! entities via identifiers is somewhat less reliable. For instance, when referencing a deployment via the trailer of its ID,
//! the system will reject the corresponding command if multiple deployments in the system end with this trailer (this is different
//! from the situation with IDs where a reference to an entity only fails if the entity does not exist).

use std::{
    borrow::{Borrow, Cow},
    fmt::Display,
};

use cell_protocol::RuntimeId;
use serde::{Deserialize, Serialize};

use crate::ExecRuntimeInfo;

use super::ids::DeploymentId;

/// Identifier referring to a deployment. A string identifies a particular deployment if it (a) exactly matches the application name or (b) matches the first `n` chars of the deployment ID,
/// with `n` being the length of the identifier string. If an identifier matches multiple deployments, it is considered ambiguous and is handled the same as if it didn't match any deployment.
#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct DeploymentIdentifier<'a>(Cow<'a, str>);

impl<T> AsDeploymentIdentifier for T
where
    T: AsRef<str> + ?Sized,
{
    fn as_depl_identifier(&self) -> DeploymentIdentifier<'_> {
        DeploymentIdentifier(Cow::Borrowed(self.as_ref()))
    }
}

impl AsDeploymentIdentifier for DeploymentIdentifier<'_> {
    fn as_depl_identifier(&self) -> DeploymentIdentifier<'_> {
        self.clone()
    }
}

impl AsDeploymentIdentifier for DeploymentId {
    fn as_depl_identifier(&self) -> DeploymentIdentifier<'_> {
        DeploymentIdentifier(Cow::Owned(self.to_string()))
    }
}

impl Display for DeploymentIdentifier<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{ident}", ident = self.0)
    }
}

pub trait AsDeploymentIdentifier {
    fn as_depl_identifier(&self) -> DeploymentIdentifier<'_>;
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
pub struct RuntimeIdentifier(pub String);

impl AsRef<str> for RuntimeIdentifier {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl RuntimeIdentifier {
    /// Returns
    /// (1) the Id of the runtime with the identifier as its name, given that the name is unique within the provided records
    /// OR
    /// (2) the Id of the runtime whose Id starts with the identifier, given that no other runtime Id within the provided records starts with this prefix
    /// OR
    /// (3) None if no runtime matches the identifier or multiple runtimes match the identifier
    #[must_use]
    pub fn identified_rt<U, T>(&self, rt_records: T) -> Option<RuntimeId>
    // method generic, so that we can use it with both `Vec<&ExecRuntimeRecord>` and `&[ExecRuntimeRecord]`
    where
        T: AsRef<[U]>,
        U: Borrow<ExecRuntimeInfo>,
    {
        let rt_records = rt_records.as_ref();
        let identifies_by_name: Vec<&ExecRuntimeInfo> = rt_records
            .iter()
            .filter_map(|rt_rec| {
                if rt_rec.borrow().name() == Some(&self.0) {
                    Some(rt_rec.borrow())
                } else {
                    None
                }
            })
            .collect();
        if identifies_by_name.len() == 1 {
            return Some(identifies_by_name[0].id());
        }

        let identifier_by_id_trailer: Vec<&ExecRuntimeInfo> = rt_records
            .iter()
            .filter_map(|rt_rec| {
                let id_string = rt_rec.borrow().id().to_string();
                if id_string.starts_with(&self.0) {
                    Some(rt_rec.borrow())
                } else {
                    None
                }
            })
            .collect();

        if identifier_by_id_trailer.len() == 1 {
            Some(identifier_by_id_trailer[0].id())
        } else {
            None
        }
    }
}
