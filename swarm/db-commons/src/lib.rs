#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

/// The system namespace. Data stored here is automatically replicated to all
/// nodes in the swarm.
pub const NAMESPACE_SYS: &str = "sys";

/// The self-organisation namespace: the cell and execution metadata that
/// orchestration and execution keep between them — registries, placements,
/// deployments, leases.
///
/// Replicated only by nodes that take part in orchestration or execution,
/// rather than by every node in the swarm.
pub const NAMESPACE_SORG: &str = "sorg";

#[cfg(feature = "models")]
pub mod models;

#[cfg(feature = "topics")]
pub mod topics;

#[cfg(feature = "query")]
pub mod query;
