mod bridge;
mod embedded;
mod gateway;
mod handler_crash;
mod lifecycle;
mod placement_routing;
mod restart;
mod tagged_placement;

use cell_protocol::{PlacementEntry, PlacementKind, RuntimeId};

fn assert_wasm_runtime_id(entry: &PlacementEntry) -> RuntimeId {
    let PlacementKind::Wasm { ref runtime } = entry.kind else {
        panic!("expected Wasm placement, got {:?}", entry.kind);
    };
    runtime.id()
}
