//! End-to-end test for exclusive single-cell ownership of the signal layer
//! (swarm#1340): the gate on the tap host functions, driven through real
//! Wasmtime instances with distinct cell identities against a live server.
//!
//! Each "cell" is a `Store<CellId>` whose `CellIdentity` returns a distinct
//! `(Sri, Gen)`; they share one linker (one `TapClient`), exactly as cells do
//! on a node.

use std::{path::PathBuf, sync::Arc};

use cell_protocol::{Gen, Sri};
use signal_layer_ipc::{StoreRead, TapStore};
use sorg_execution::wasm_tap::{CellIdentity, link_tap_functions};
use tempfile::TempDir;
use tokio::net::UnixListener;
use wasmtime::{Config, Engine, Linker, Module, Store};

/// Per-cell store state carrying a signal-layer identity.
struct CellId(Option<(Sri, Gen)>);

impl CellIdentity for CellId {
    fn sl_identity(&self) -> Option<(Sri, Gen)> {
        self.0
    }
}

fn id(sri_byte: u8, gen_time: u64) -> (Sri, Gen) {
    let mut bytes = [0u8; 16];
    bytes[0] = sri_byte;
    (
        Sri::from_uuid(uuid::Uuid::from_bytes(bytes)),
        Gen::from_parts(gen_time, 1),
    )
}

fn cell(sri_byte: u8, gen_time: u64) -> CellId {
    CellId(Some(id(sri_byte, gen_time)))
}

/// Minimal tap store: one resolvable tap named "temp".
struct OneTap;

impl TapStore for OneTap {
    fn resolve(&self, name: &str) -> Option<u32> {
        (name == "temp").then_some(1)
    }
    fn type_id(&self, h: u32) -> Option<u32> {
        (h == 1).then_some(0xF32)
    }
    fn read_retained(&self, _h: u32) -> StoreRead {
        StoreRead::Empty
    }
    fn take_event(&self, _h: u32) -> StoreRead {
        StoreRead::Empty
    }
    fn list_len(&self) -> u32 {
        1
    }
    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        (index == 0).then(|| ("temp".to_owned(), 0))
    }
}

const WAT: &str = r#"
(module
  (import "tap" "tap_resolve" (func $tap_resolve (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "tap_resolve") (param i32 i32) (result i32)
    local.get 0  local.get 1  call $tap_resolve)
)
"#;

fn engine() -> Engine {
    let mut config = Config::new();
    config.consume_fuel(true);
    Engine::new(&config).expect("engine")
}

fn spawn_server(dir: &TempDir) -> PathBuf {
    let path = dir.path().join("tap.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    tokio::spawn(signal_layer_ipc::serve(listener, Arc::new(OneTap), None));
    path
}

/// Call `tap_resolve("temp")` from a cell's own store; returns the raw host
/// status (virtual handle ≥ 1, or a negative error code).
async fn resolve_temp(engine: &Engine, linker: &Linker<CellId>, mut store: Store<CellId>) -> i32 {
    store.set_fuel(u64::MAX).unwrap();
    let module = Module::new(engine, WAT).expect("module");
    let instance = linker
        .instantiate_async(&mut store, &module)
        .await
        .expect("instantiate");
    // Write "temp" at offset 0.
    let mem = instance.get_memory(&mut store, "memory").expect("memory");
    mem.write(&mut store, 0, b"temp").expect("write");
    let f = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "tap_resolve")
        .expect("func");
    f.call_async(&mut store, (0, 4)).await.expect("call")
}

/// EACCES — the code a non-owner cell receives (must match `sl_claim::SL_CLAIMED`).
const EACCES: i32 = -13;

#[tokio::test]
async fn first_cell_claims_second_is_denied_then_reclaims_after_release() {
    let dir = TempDir::new().unwrap();
    let path = spawn_server(&dir);
    let client = Arc::new(signal_layer_ipc::TapClient::new(path));

    let eng = engine();
    let mut linker: Linker<CellId> = Linker::new(&eng);
    link_tap_functions(&mut linker, client).expect("link");

    // Cell A resolves first → claims the SL, gets a real handle.
    let a = resolve_temp(&eng, &linker, Store::new(&eng, cell(1, 10))).await;
    assert!(a >= 1, "owner cell must resolve, got {a}");

    // Cell A again → still allowed.
    let a2 = resolve_temp(&eng, &linker, Store::new(&eng, cell(1, 10))).await;
    assert!(a2 >= 1, "owner cell must keep access, got {a2}");

    // Cell B (different identity) → refused with EACCES, no handle.
    let b = resolve_temp(&eng, &linker, Store::new(&eng, cell(2, 20))).await;
    assert_eq!(
        b, EACCES,
        "a second cell must be refused while A owns the SL"
    );

    // A is destroyed → release its claim (what CellMessageHandler::drop does).
    let (a_sri, a_gen) = id(1, 10);
    sorg_execution::wasm_tap::release_sl_claim(a_sri, a_gen);

    // Now cell B can claim.
    let b2 = resolve_temp(&eng, &linker, Store::new(&eng, cell(2, 20))).await;
    assert!(
        b2 >= 1,
        "after the owner releases, the next cell claims, got {b2}"
    );
}
