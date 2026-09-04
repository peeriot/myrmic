//! Exec-side supervision: node-lease renewal (this module) and the
//! child-side fencing verification pass (`fencing`).

pub(crate) mod fencing;
mod renewal;
pub(crate) mod startup;

pub(crate) use renewal::spawn_renewal;
