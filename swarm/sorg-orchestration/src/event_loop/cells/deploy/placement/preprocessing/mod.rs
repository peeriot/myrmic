use std::collections::HashMap;

use cell_protocol::{ArtifactPlatform, ClassInfo, RuntimeId, RuntimeKind, Sri};
use sorg_common::{
    ArtifactKind, CellConfig, CellDeployment, CellInfeasibility, ExecRuntimeInfo, RejectionReason,
    RuntimeRejection, TagRequirement, check_tag_requirements,
};

use super::PlacementContext;

type CellsPerRuntime = HashMap<RuntimeId, Vec<Sri>>;

pub(super) struct CellMapping {
    pub(super) sri: Sri,
    pub(super) options: PlacementOptions,
}

pub(super) enum PlacementOptions {
    Infeasible(Vec<RuntimeRejection>),
    Trivial(RuntimeId),
    Untrivial(Vec<RuntimeId>),
}

pub(super) enum CellMappings {
    Infeasible(Vec<CellInfeasibility>),
    Trivial(Vec<CellMapping>),
    Untrivial(Vec<CellMapping>),
}

pub(super) fn preprocess(cells: &[CellDeployment], context: &PlacementContext) -> CellMappings {
    let mappings: Vec<CellMapping> = cells
        .iter()
        .map(|cell| mapping_for_cell(cell, context))
        .collect();

    let infeasible: Vec<CellInfeasibility> = mappings
        .iter()
        .filter_map(|m| {
            if let PlacementOptions::Infeasible(rejections) = &m.options {
                Some(CellInfeasibility {
                    cell: m.sri,
                    rejections: rejections.clone(),
                })
            } else {
                None
            }
        })
        .collect();

    if !infeasible.is_empty() {
        return CellMappings::Infeasible(infeasible);
    }

    let has_untrivial = mappings
        .iter()
        .any(|m| matches!(m.options, PlacementOptions::Untrivial(_)));

    if has_untrivial {
        CellMappings::Untrivial(mappings)
    } else {
        CellMappings::Trivial(mappings)
    }
}

/// Maps a single cell to its placement options. Assumes at least one runtime is
/// present — the empty-runtimes case is handled by the caller as
/// `DeploymentError::NoRuntimesAvailable`.
fn mapping_for_cell(cell: &CellDeployment, context: &PlacementContext) -> CellMapping {
    let mut eligible = Vec::new();
    let mut rejections = Vec::new();

    for rt in context.execs() {
        let rejection = tags_rejection(rt, cell)
            .or_else(|| artifact_rejection(cell, rt, context.class_info()))
            .or_else(|| capacity_rejection(rt, context.cells_per_runtime()));

        match rejection {
            Some(reason) => rejections.push(RuntimeRejection {
                runtime: rt.id(),
                reason,
            }),
            None => eligible.push(rt.id()),
        }
    }

    let options = match eligible.len() {
        0 => PlacementOptions::Infeasible(rejections),
        1 => PlacementOptions::Trivial(eligible[0]),
        _ => PlacementOptions::Untrivial(eligible),
    };

    CellMapping {
        sri: cell.sri,
        options,
    }
}

/// Returns `Some(MissingTags)` if the runtime does not satisfy the cell's tag
/// requirements, `None` if all tags are met.
fn tags_rejection(runtime: &ExecRuntimeInfo, cell: &CellDeployment) -> Option<RejectionReason> {
    match check_tag_requirements(runtime.capabilities(), cell.tags()) {
        TagRequirement::Unmet { missing } => Some(RejectionReason::MissingTags(missing)),
        TagRequirement::Met => None,
    }
}

/// Returns `Some(reason)` if the runtime cannot load the cell because the
/// required artifact is missing, or `None` if the check passes (or does not
/// apply — bridges have no class artifacts).
fn artifact_rejection(
    cell: &CellDeployment,
    runtime: &ExecRuntimeInfo,
    class_info: &HashMap<String, ClassInfo>,
) -> Option<RejectionReason> {
    let CellConfig::Wasm { ref class } = cell.config else {
        return None;
    };
    let info = class_info.get(class);

    match runtime.runtime_kind() {
        RuntimeKind::Linux => {
            let has_wasm = info.is_some_and(|i| i.wasm_hash.is_some());
            if has_wasm {
                None
            } else {
                Some(RejectionReason::MissingArtifact(ArtifactKind::Wasm))
            }
        }
        RuntimeKind::Esp32c5 | RuntimeKind::Esp32c6 | RuntimeKind::Esp32c61 => {
            let has_aot = info.is_some_and(|i| {
                i.artifacts
                    .iter()
                    .any(|a| a.platform == ArtifactPlatform::Riscv32imac)
            });
            if has_aot {
                None
            } else {
                Some(RejectionReason::MissingArtifact(ArtifactKind::Aot {
                    target: ArtifactPlatform::Riscv32imac,
                }))
            }
        }
        RuntimeKind::Unknown => Some(RejectionReason::UnsupportedRuntime),
    }
}

/// Returns `Some(AtCapacity)` if the runtime is embedded and already hosts at
/// least one cell, `None` if the runtime has free capacity.
fn capacity_rejection(
    runtime: &ExecRuntimeInfo,
    cells_per_runtime: &CellsPerRuntime,
) -> Option<RejectionReason> {
    let is_embedded = runtime.runtime_kind().is_embedded();
    if is_embedded && cells_per_runtime.contains_key(&runtime.id()) {
        Some(RejectionReason::AtCapacity)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
