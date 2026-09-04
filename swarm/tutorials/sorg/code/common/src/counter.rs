use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Counter {
    count: u32,
}

impl Counter {
    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn increment(&mut self) {
        self.count += 1;
    }

    pub fn from_payload(payload: &[u8]) -> Result<Self> {
        bincode::deserialize(payload).context("error deserializing counter")
    }

    pub fn to_payload(&self) -> Result<Vec<u8>> {
        bincode::serialize(&self).context("failed to serialize counter")
    }
}
