//! Runtime-family scaffold modules.
//!
//! `embassy` — default Embassy token blocks, verbatim copies of the inline
//! `quote!` blocks currently in the emit files.
//!
//! `tokio` — Linux tokio equivalents for each hook.

pub mod embassy;
pub mod tokio;
