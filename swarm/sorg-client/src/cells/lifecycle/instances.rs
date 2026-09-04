use cell_protocol::{CellInstance, Sri};
use sorg_common::instance_registry;

use sorg_common::bail;

use crate::{Client, Result};

impl Client {
    /// Returns all created cell instances.
    pub async fn list_instances(&self) -> Result<Vec<CellInstance>> {
        Ok(instance_registry::list_instances(self.session()).await?)
    }

    /// Erases a cell instance from the datalayer.
    pub async fn erase_instance(&self, sri: &Sri) -> Result<()> {
        Ok(instance_registry::erase_instance(self.session(), sri).await?)
    }

    /// Erases a cell instance if it still exists, returning whether a row
    /// was deleted. Tolerates rows undeploy has already erased.
    pub async fn erase_instance_if_present(&self, sri: &Sri) -> Result<bool> {
        Ok(instance_registry::erase_instance_if_present(self.session(), sri).await?)
    }

    /// Returns the stored info for a single instance.
    pub async fn inspect_instance(&self, sri: &Sri) -> Result<CellInstance> {
        let info = instance_registry::get_instance(self.session(), sri).await?;
        match info {
            Some(info) => Ok(info),
            None => bail!("instance '{}' not found", sri),
        }
    }
}
