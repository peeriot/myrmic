//! Process-global handoff of the generated outlet store to the IPC server.
//!
//! The generated pipeline builds its registries in two separate functions with
//! stable signatures (`setup_outlet_registry() -> usize`, then
//! `setup_tap_registry() -> usize`, which starts the IPC server). The outlet
//! store is created in the first and consumed in the second, so it is parked
//! here in between. `take` consumes the parked store; if the setup order is
//! violated the server starts without one and answers outlet requests with
//! `Unsupported`.

use std::sync::{Arc, Mutex};

use signal_layer_ipc::OutletStore;

static PARKED: Mutex<Option<Arc<dyn OutletStore>>> = Mutex::new(None);

/// Park the outlet store for the IPC server to pick up.
///
/// Called by the generated `setup_outlet_registry()`. A second call replaces
/// the parked store (harmless: the generated code calls it exactly once).
pub fn set_outlet_store(store: Arc<dyn OutletStore>) {
    *PARKED.lock().expect("outlet store lock poisoned") = Some(store);
}

/// Take the parked outlet store, leaving `None`.
///
/// Called by the generated `setup_tap_registry()` when starting the IPC
/// server. Returns `None` if no store was parked (sensors-only pipeline, or
/// setup order violated) — the server then answers outlet requests with
/// `Unsupported`.
pub fn take_outlet_store() -> Option<Arc<dyn OutletStore>> {
    PARKED.lock().expect("outlet store lock poisoned").take()
}

#[cfg(test)]
mod tests {
    use signal_layer_ipc::StoreWrite;

    use super::*;

    struct NullStore;

    impl OutletStore for NullStore {
        fn resolve(&self, _name: &str) -> Option<u32> {
            None
        }
        fn write(&self, _h: u32, _bytes: &[u8]) -> StoreWrite {
            StoreWrite::InvalidHandle
        }
        fn list_len(&self) -> u32 {
            0
        }
        fn list_entry(&self, _index: u32) -> Option<(String, u8)> {
            None
        }
        fn type_id(&self, _h: u32) -> Option<u32> {
            None
        }
    }

    #[test]
    fn set_then_take_consumes_the_store() {
        set_outlet_store(Arc::new(NullStore));
        assert!(take_outlet_store().is_some(), "parked store must be taken");
        assert!(take_outlet_store().is_none(), "take must consume");
    }
}
