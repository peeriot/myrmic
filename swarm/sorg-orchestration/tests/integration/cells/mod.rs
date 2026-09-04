mod bridge;
mod embedded;
mod lifecycle;
mod tagged_placement;

use cell_protocol::{PlacementEntry, PlacementKind, RuntimeId};

fn assert_wasm_runtime_id(entry: &PlacementEntry) -> RuntimeId {
    let PlacementKind::Wasm { ref runtime } = entry.kind else {
        panic!("expected Wasm placement, got {:?}", entry.kind);
    };
    runtime.id()
}
