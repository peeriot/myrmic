//! Crate for the code shared between wasm test modules and the integration tests using these modules.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

type Result<T> = core::result::Result<T, &'static str>;

/// A simple type used as the payload for interacting with/testing of wasm modules
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Counter {
    count: u32,
}

impl Counter {
    #[must_use]
    pub fn new(count: u32) -> Self {
        Self { count }
    }

    #[must_use]
    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn increment(&mut self) {
        self.count += 1;
    }

    pub fn increment_by(&mut self, value: u32) {
        self.count += value;
    }

    pub fn from_payload(bytes: &[u8]) -> Result<Self> {
        let (value, _rest) =
            postcard::take_from_bytes(bytes).map_err(|_| "failed to deserialize counter")?;
        Ok(value)
    }

    pub fn serialize_into(&self, buffer: &mut [u8]) -> Result<usize> {
        let remaining = postcard::to_slice(self, buffer).map_err(|_| "serialization error")?;
        Ok(remaining.len())
    }

    pub fn to_payload(&self) -> Result<Vec<u8>> {
        let bytes = postcard::to_allocvec(self).map_err(|_| "failed to serialize counter")?;
        Ok(bytes)
    }
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Clone, Copy)]
pub struct Temperature {
    pub degrees_celsius: i32,
}

// we will want to generate this
impl Temperature {
    pub fn new(degrees_celsius: i32) -> Self {
        Self { degrees_celsius }
    }

    pub fn from_payload(bytes: &[u8]) -> Result<Self> {
        let (value, _) =
            postcard::take_from_bytes(bytes).map_err(|_| "failed to deserialize temperature")?;
        Ok(value)
    }

    pub fn to_payload(&self) -> Result<Vec<u8>> {
        postcard::to_allocvec(self).map_err(|_| "failed to serialize temperature")
    }
}
