//! Re-export shim: all descriptor types now live in `pipeline-backend-api`.
//! This module re-exports everything so existing `crate::descriptor::*` paths
//! in emit files, validate.rs, and test code continue to compile unchanged.

pub use pipeline_backend_api::descriptor::{
    ConfigField, DriverInput, DriverOutput, DriverSchema, DriverWrite, OutputMode, RequiredBus,
    Requires, Scope, load_schema_from_yaml,
};
