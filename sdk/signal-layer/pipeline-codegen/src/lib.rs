pub mod cargo_update;
pub mod descriptor;
pub mod emit;
pub mod manifest;
pub mod pipeline;
pub mod validate;

// Re-export `ChipBackend` and model types from the API crate so all existing
// import paths continue to compile unchanged.
pub use pipeline_backend_api::ChipBackend;

/// Generate a formatted Rust source file from a board manifest + pipeline.
/// Convenience re-export of [`emit::generate`].
pub use emit::generate;
