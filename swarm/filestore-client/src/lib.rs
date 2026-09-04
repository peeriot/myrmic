mod client;
mod error;
mod topics;
mod types;

pub use client::Client;
pub use error::{Error, Result};
pub use topics::*;
pub use types::*;

pub use db_client::v1::models::Scope;
