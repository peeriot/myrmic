//! Chip-agnostic codegen backend trait, shared model types, and runtime scaffolds.
//!
//! Crate layout:
//! - `ChipBackend` — the trait every backend implements (re-exported from root).
//! - `manifest` — `BoardManifest` and related board description types.
//! - `descriptor` — `DriverSchema` and related driver descriptor types.
//! - `validate_types` — `ValidationError`.
//! - `scaffold::embassy` — Embassy token-block defaults for the ten runtime hooks.
//! - `scaffold::tokio` — Linux tokio equivalents.

pub mod backend;
pub mod descriptor;
pub mod manifest;
pub mod scaffold;
pub mod validate_types;

pub use backend::ChipBackend;
pub use validate_types::ValidationError;
