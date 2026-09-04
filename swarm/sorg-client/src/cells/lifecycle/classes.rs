use cell_protocol::{AddMode, ClassArtifact, ClassInfo};
use sorg_common::class_registry;

use crate::{Client, Result};

impl Client {
    /// Returns all registered cell classes.
    pub async fn list_classes(&self) -> Result<Vec<ClassInfo>> {
        Ok(class_registry::list_classes(self.session()).await?)
    }

    /// Returns detailed info for a single class, or `None` if not found.
    pub async fn get_class_info(&self, name: &str) -> Result<Option<ClassInfo>> {
        Ok(class_registry::get_class_info(self.session(), name).await?)
    }

    /// Removes a cell class from the datalayer.
    pub async fn remove_class(&self, name: &str) -> Result<()> {
        Ok(class_registry::remove_class(self.session(), name).await?)
    }

    /// Adds an artifact to a cell class, creating the class if it doesn't exist.
    pub async fn add_class_artifact(
        &self,
        name: &str,
        artifact: ClassArtifact,
        mode: AddMode,
    ) -> Result<ClassInfo> {
        Ok(class_registry::add_class_artifact(self.session(), name, artifact, mode).await?)
    }
}
