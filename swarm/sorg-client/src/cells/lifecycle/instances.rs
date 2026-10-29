use cell_protocol::{CellInstance, Gen, Sri};
use sorg_common::{FenceOutcome, instance_registry};

use sorg_common::bail;

use crate::{Client, Result};

impl Client {
    /// Returns all created cell instances.
    pub async fn list_instances(&self) -> Result<Vec<CellInstance>> {
        Ok(instance_registry::list_instances(self.session()).await?)
    }

    /// Removes the instance row of generation `gen_id`; a newer generation is
    /// never touched, and the outcome says whether `gen_id` was the newest.
    /// Refused while that incarnation is deployed; an absent row is an outcome,
    /// not an error.
    pub async fn remove_instance(&self, sri: &Sri, gen_id: Gen) -> Result<FenceOutcome> {
        Ok(instance_registry::remove_instance(self.session(), sri, gen_id).await?)
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
