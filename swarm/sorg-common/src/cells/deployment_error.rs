//! Structured error describing why a cell deployment did not succeed.
//!
//! Travels on the deploy/load query reply (postcard-serialized) so the client
//! can match on the failure instead of parsing a string. The orchestrator
//! decides placement first and only then attempts deployment, so a placement
//! failure ([`DeploymentError::Infeasible`] / [`DeploymentError::NoRuntimesAvailable`])
//! is mutually exclusive with a deploy-phase failure ([`DeploymentError::DeploymentFailed`]).

use std::fmt::{self, Display};

use cell_protocol::{ArtifactPlatform, RuntimeId, Sri};
use serde::{Deserialize, Serialize};

/// Why a cell deployment failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeploymentError {
    /// No execution runtimes are registered at all.
    NoRuntimesAvailable,
    /// Runtimes exist, but one or more cells could not be placed on any of them.
    /// Carries one entry per unplaceable cell.
    Infeasible(Vec<CellInfeasibility>),
    /// Every cell has at least one eligible runtime, but no valid joint assignment
    /// exists — the cells conflict with each other over shared constrained resources
    /// (e.g. capacity-1 embedded runtimes).
    PlacementConflicts,
    /// Every cell was placed, deployment was attempted, but one or more cells
    /// failed on their runtime.
    DeploymentFailed(Vec<CellFailure>),
    /// No orchestrator responded to the request.
    OrchestratorUnreachable,
    /// The deployment request carried no cells.
    EmptyDeployment,
    /// An application with this name is already deployed.
    DuplicateAppName { name: String },
    /// A cell with this SRI is already deployed.
    DuplicateSri { sri: Sri },
    /// The referenced cell class is not present in the class registry.
    UnknownClass { class: String },
    /// The Zenoh query to the orchestrator timed out before a reply arrived.
    QueryTimeout,
    /// An orchestrator-internal failure (datalayer error, serialization, …).
    Internal(String),
}

/// Why a single cell could not be placed on any runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellInfeasibility {
    /// The cell that could not be placed.
    pub cell: Sri,
    /// One rejection per candidate runtime, explaining why it cannot host the cell.
    pub rejections: Vec<RuntimeRejection>,
}

/// A single runtime's reason for not being able to host a cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRejection {
    /// The runtime that was rejected as a target.
    pub runtime: RuntimeId,
    /// Why this runtime cannot host the cell.
    pub reason: RejectionReason,
}

/// The reason a runtime was rejected as a placement target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectionReason {
    /// The runtime lacks one or more of the cell's required tags.
    MissingTags(Vec<String>),
    /// The artifact this runtime would need to load is not present in the datalayer.
    MissingArtifact(ArtifactKind),
    /// The runtime is already hosting a cell and cannot take another
    /// (embedded runtimes host a single cell at a time).
    AtCapacity,
    /// The runtime kind is not recognised — the orchestrator has no deploy path for it.
    UnsupportedRuntime,
}

/// Which artifact a runtime requires to load a cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactKind {
    /// A linux runtime loads the wasm blob.
    Wasm,
    /// An embedded runtime loads the AOT artifact for its target.
    Aot { target: ArtifactPlatform },
}

/// A single cell's failure during the deploy phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellFailure {
    /// The cell that failed to deploy.
    pub cell: Sri,
    /// The runtime the cell was placed on.
    pub runtime: RuntimeId,
    /// How the deployment failed.
    pub kind: CellFailureKind,
}

/// How a cell's deployment failed on its runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CellFailureKind {
    /// The runtime accepted the request but reported a failure.
    RuntimeReported(String),
    /// The runtime never confirmed the deployment within the deadline.
    Timeout,
}

impl std::error::Error for DeploymentError {}

impl Display for DeploymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRuntimesAvailable => write!(f, "no runtimes available"),
            Self::OrchestratorUnreachable => {
                write!(f, "no response from an orchestration runtime")
            }
            Self::QueryTimeout => write!(f, "query to orchestrator timed out"),
            Self::EmptyDeployment => write!(f, "deployment request has an empty cells list"),
            Self::DuplicateAppName { name } => {
                write!(f, "application '{name}' is already deployed")
            }
            Self::DuplicateSri { sri } => write!(f, "cell '{sri}' is already deployed"),
            Self::UnknownClass { class } => {
                write!(f, "class '{class}' not found in class registry")
            }
            Self::Internal(msg) => write!(f, "{msg}"),
            Self::PlacementConflicts => write!(
                f,
                "no valid placement exists: cells conflict over shared constrained resources"
            ),
            Self::Infeasible(cells) => {
                write!(f, "deployment infeasible:")?;
                for cell in cells {
                    write!(f, "\n  cell '{}' cannot be placed:", cell.cell)?;
                    for rejection in &cell.rejections {
                        write!(
                            f,
                            "\n    - runtime {}: {}",
                            rejection.runtime, rejection.reason
                        )?;
                    }
                }
                Ok(())
            }
            Self::DeploymentFailed(failures) => {
                write!(f, "deployment failed:")?;
                for failure in failures {
                    write!(
                        f,
                        "\n  cell '{}' failed on runtime {}: {}",
                        failure.cell, failure.runtime, failure.kind
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl Display for RejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTags(tags) => {
                write!(f, "missing required tags [{}]", tags.join(", "))
            }
            Self::MissingArtifact(kind) => write!(f, "missing required artifact: {kind}"),
            Self::AtCapacity => write!(f, "runtime already at capacity"),
            Self::UnsupportedRuntime => write!(f, "runtime kind is not supported"),
        }
    }
}

impl Display for ArtifactKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wasm => write!(f, "wasm"),
            Self::Aot { target } => write!(f, "aot for {target}"),
        }
    }
}

impl Display for CellFailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeReported(msg) => write!(f, "{msg}"),
            Self::Timeout => write!(f, "timed out waiting for deployment confirmation"),
        }
    }
}
