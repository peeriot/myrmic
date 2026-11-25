use cell_protocol::{ArtifactPlatform, BlobHash, ClassInfo};
use sorg_common::class_registry::{self, Removal};

use crate::{Client, Result};

impl Client {
    /// Returns all registered cell classes with their ids.
    pub async fn list_classes(&self) -> Result<Vec<(BlobHash, ClassInfo)>> {
        Ok(class_registry::list_classes(self.session()).await?)
    }

    /// Returns every bound class name with the class it points to.
    pub async fn list_class_names(&self) -> Result<Vec<(String, BlobHash)>> {
        Ok(class_registry::list_names(self.session()).await?)
    }

    /// Returns the class a name points to, or `None` if it is not bound.
    pub async fn resolve_class_name(&self, name: &str) -> Result<Option<BlobHash>> {
        Ok(class_registry::resolve_name(self.session(), name).await?)
    }

    /// Returns a class by id, or `None` if not found.
    pub async fn get_class_info(&self, id: BlobHash) -> Result<Option<ClassInfo>> {
        Ok(class_registry::get_class_info(self.session(), id).await?)
    }

    /// Registers a wasm class with its AOT compilations and binds `name` to it.
    /// Returns the class id.
    pub async fn add_class(
        &self,
        name: &str,
        wasm: Vec<u8>,
        aots: Vec<(ArtifactPlatform, Vec<u8>)>,
    ) -> Result<BlobHash> {
        Ok(class_registry::add_class(self.session(), name, wasm, aots).await?)
    }

    /// Adds an AOT binary under `name`: to the wasm class it points to, else as
    /// a class of its own. Returns the AOT's id.
    pub async fn add_class_aot(
        &self,
        name: &str,
        platform: ArtifactPlatform,
        aot: Vec<u8>,
    ) -> Result<BlobHash> {
        Ok(class_registry::add_aot(self.session(), name, platform, aot).await?)
    }

    /// Unbinds `name` if it points to class `id`. Returns whether it did.
    pub async fn unbind_class_name(&self, name: &str, id: BlobHash) -> Result<bool> {
        Ok(class_registry::unbind_name(self.session(), name, id).await?)
    }

    /// Removes class `id` unless something still references it.
    pub async fn remove_class_if_unreferenced(&self, id: BlobHash) -> Result<Removal> {
        Ok(class_registry::remove_if_unreferenced(self.session(), id).await?)
    }
}
