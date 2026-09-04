//! Integration tests for the four outlet host functions in sorg-execution.
//!
//! Same shape as `tap_host_functions.rs`: a WAT module imports the outlet host
//! functions and re-exports them as plain wrappers — exactly what a real cell
//! does — driven through a minimal Wasmtime instance against an in-process IPC
//! server on a tempdir socket.
//!
//! Memory layout: page 0 of a single 64 `KiB` page.
//!   [0   ..  127]  name / payload scratch
//!   [256 ..  511]  name-out buffer (`list_entry`)
//!   [512 ..  515]  i32 kind out (`out_kind_ptr`)

use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;
use tokio::net::UnixListener;

use myrmic_common::types::error::{EINVAL, ESTALE};
use signal_layer_ipc::{OutletStore, StoreRead, StoreWrite, TapStore};
use wasmtime::{Config, Engine, Instance, Linker, Module, Store};

// ── Stub stores ───────────────────────────────────────────────────────────────

/// Tap store with nothing in it — outlet tests only need the server to run.
struct EmptyTapStore;

impl TapStore for EmptyTapStore {
    fn resolve(&self, _name: &str) -> Option<u32> {
        None
    }
    fn type_id(&self, _h: u32) -> Option<u32> {
        None
    }
    fn read_retained(&self, _h: u32) -> StoreRead {
        StoreRead::InvalidHandle
    }
    fn take_event(&self, _h: u32) -> StoreRead {
        StoreRead::InvalidHandle
    }
    fn list_len(&self) -> u32 {
        0
    }
    fn list_entry(&self, _index: u32) -> Option<(String, u8)> {
        None
    }
}

/// One outlet `relay_cmd` (server handle 1, kind 0); the only payload it
/// decodes is `OK_PAYLOAD` (stand-in for the OUT-08 typed-decode check).
struct StubOutletStore;

const OUTLET_NAME: &str = "relay_cmd";
/// Wire-type id the stub outlet reports (arbitrary).
const OUTLET_TYPE_ID: u32 = 0xD16;
const OK_PAYLOAD: &[u8] = &[0x01];

impl OutletStore for StubOutletStore {
    fn resolve(&self, name: &str) -> Option<u32> {
        (name == OUTLET_NAME).then_some(1)
    }

    fn type_id(&self, h: u32) -> Option<u32> {
        (h == 1).then_some(OUTLET_TYPE_ID)
    }

    fn write(&self, h: u32, bytes: &[u8]) -> StoreWrite {
        if h != 1 {
            return StoreWrite::InvalidHandle;
        }
        if bytes == OK_PAYLOAD {
            StoreWrite::Ok
        } else {
            StoreWrite::Rejected
        }
    }

    fn list_len(&self) -> u32 {
        1
    }

    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        (index == 0).then(|| (OUTLET_NAME.to_owned(), 0))
    }
}

// ── Harness ───────────────────────────────────────────────────────────────────

/// WAT module importing the four outlet host functions, re-exported as plain
/// wrappers — the pattern a real cell uses.
const OUTLET_WRAPPER_WAT: &str = r#"
(module
  (import "outlet" "outlet_resolve"
    (func $outlet_resolve (param i32 i32) (result i32)))
  (import "outlet" "outlet_write_retained"
    (func $outlet_write_retained (param i32 i32 i32) (result i32)))
  (import "outlet" "outlet_list_len"
    (func $outlet_list_len (result i32)))
  (import "outlet" "outlet_list_entry"
    (func $outlet_list_entry (param i32 i32 i32 i32) (result i32)))
  (import "outlet" "outlet_type_id"
    (func $outlet_type_id (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)

  (func (export "outlet_resolve") (param i32 i32) (result i32)
    local.get 0  local.get 1  call $outlet_resolve)
  (func (export "outlet_write_retained") (param i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  call $outlet_write_retained)
  (func (export "outlet_list_len") (result i32)
    call $outlet_list_len)
  (func (export "outlet_list_entry") (param i32 i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  local.get 3
    call $outlet_list_entry)
  (func (export "outlet_type_id") (param i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  call $outlet_type_id)
)
"#;

fn spawn_server(dir: &TempDir) -> PathBuf {
    let path = dir.path().join("outlet.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    let taps: Arc<dyn TapStore> = Arc::new(EmptyTapStore);
    let outlets: Arc<dyn OutletStore> = Arc::new(StubOutletStore);
    tokio::spawn(signal_layer_ipc::serve(listener, taps, Some(outlets)));
    path
}

async fn build_instance(path: PathBuf) -> (Store<()>, Instance) {
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config).expect("engine");

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let mut linker: Linker<()> = Linker::new(&engine);
    sorg_execution::wasm_tap::link_outlet_functions(&mut linker, client)
        .expect("link outlet functions");

    let mut store = Store::new(&engine, ());
    store.set_fuel(u64::MAX).expect("set fuel");
    let module = Module::new(&engine, OUTLET_WRAPPER_WAT).expect("wrapper module");
    let instance = linker
        .instantiate_async(&mut store, &module)
        .await
        .expect("instantiate");
    (store, instance)
}

fn write_bytes(store: &mut Store<()>, instance: &Instance, offset: usize, b: &[u8]) {
    let mem = instance.get_memory(&mut *store, "memory").expect("memory");
    mem.write(&mut *store, offset, b).expect("write");
}

fn read_bytes(store: &mut Store<()>, instance: &Instance, offset: usize, n: usize) -> Vec<u8> {
    let mem = instance.get_memory(&mut *store, "memory").expect("memory");
    let mut buf = vec![0u8; n];
    mem.read(&*store, offset, &mut buf).expect("read");
    buf
}

fn read_i32(store: &mut Store<()>, instance: &Instance, offset: usize) -> i32 {
    let b = read_bytes(store, instance, offset, 4);
    i32::from_le_bytes(b.try_into().unwrap())
}

async fn resolve(store: &mut Store<()>, instance: &Instance, name: &str) -> i32 {
    write_bytes(store, instance, 0, name.as_bytes());
    let f = instance
        .get_typed_func::<(i32, i32), i32>(&mut *store, "outlet_resolve")
        .expect("get outlet_resolve");
    f.call_async(&mut *store, (0, i32::try_from(name.len()).unwrap()))
        .await
        .expect("call outlet_resolve")
}

async fn write_retained(
    store: &mut Store<()>,
    instance: &Instance,
    handle: i32,
    payload: &[u8],
) -> i32 {
    write_bytes(store, instance, 0, payload);
    let f = instance
        .get_typed_func::<(i32, i32, i32), i32>(&mut *store, "outlet_write_retained")
        .expect("get outlet_write_retained");
    f.call_async(
        &mut *store,
        (handle, 0, i32::try_from(payload.len()).unwrap()),
    )
    .await
    .expect("call outlet_write_retained")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn resolve_known_outlet_returns_positive_handle() {
    let dir = TempDir::new().unwrap();
    let (mut store, instance) = build_instance(spawn_server(&dir)).await;

    let vh = resolve(&mut store, &instance, OUTLET_NAME).await;
    assert!(vh >= 1, "expected handle >= 1, got {vh}");
}

#[tokio::test]
async fn resolve_unknown_outlet_returns_minus_one() {
    let dir = TempDir::new().unwrap();
    let (mut store, instance) = build_instance(spawn_server(&dir)).await;

    let vh = resolve(&mut store, &instance, "nonexistent").await;
    assert_eq!(vh, -1);
}

#[tokio::test]
async fn write_accepted_rejected_and_stale_handle() {
    let dir = TempDir::new().unwrap();
    let (mut store, instance) = build_instance(spawn_server(&dir)).await;

    let vh = resolve(&mut store, &instance, OUTLET_NAME).await;
    assert!(vh >= 1);

    let ok = write_retained(&mut store, &instance, vh, OK_PAYLOAD).await;
    assert_eq!(ok, 0, "a decodable payload must return 0 (success)");

    let rejected = write_retained(&mut store, &instance, vh, &[0xFF, 0xFF]).await;
    assert_eq!(
        rejected, EINVAL,
        "a refused payload must return EINVAL (OUT-08), matching the WAMR host"
    );

    let stale = write_retained(&mut store, &instance, 9999, OK_PAYLOAD).await;
    assert_eq!(stale, ESTALE, "a never-issued handle must return ESTALE");
}

#[tokio::test]
async fn write_negative_and_oversized_buf_len_return_minus_one() {
    let dir = TempDir::new().unwrap();
    let (mut store, instance) = build_instance(spawn_server(&dir)).await;

    let vh = resolve(&mut store, &instance, OUTLET_NAME).await;
    assert!(vh >= 1);

    let f = instance
        .get_typed_func::<(i32, i32, i32), i32>(&mut store, "outlet_write_retained")
        .expect("get outlet_write_retained");
    let negative = f
        .call_async(&mut store, (vh, 0, -1))
        .await
        .expect("call with negative len");
    assert_eq!(negative, -1, "negative buf_len must return -1");

    let oversized_len =
        i32::try_from(signal_layer_ipc::MAX_OUTLET_WRITE_LEN + 1).expect("bound fits i32");
    let oversized = f
        .call_async(&mut store, (vh, 0, oversized_len))
        .await
        .expect("call with oversized len");
    assert_eq!(
        oversized, -1,
        "a buf_len past the frame bound must be rejected in the host function"
    );
}

#[tokio::test]
async fn list_len_and_entry_roundtrip() {
    let dir = TempDir::new().unwrap();
    let (mut store, instance) = build_instance(spawn_server(&dir)).await;

    let len_fn = instance
        .get_typed_func::<(), i32>(&mut store, "outlet_list_len")
        .expect("get outlet_list_len");
    assert_eq!(len_fn.call_async(&mut store, ()).await.expect("call"), 1);

    let entry_fn = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "outlet_list_entry")
        .expect("get outlet_list_entry");
    let n = entry_fn
        .call_async(&mut store, (0, 256, 64, 512))
        .await
        .expect("call entry 0");
    assert_eq!(n, i32::try_from(OUTLET_NAME.len()).unwrap());
    assert_eq!(
        read_bytes(&mut store, &instance, 256, OUTLET_NAME.len()),
        OUTLET_NAME.as_bytes()
    );
    assert_eq!(
        read_i32(&mut store, &instance, 512),
        0,
        "kind widened u8→i32"
    );

    let oob = entry_fn
        .call_async(&mut store, (99, 256, 64, 512))
        .await
        .expect("call entry 99");
    assert_eq!(oob, -1, "out-of-range index must return -1");
}

#[tokio::test]
async fn server_down_resolve_returns_minus_one() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("no_server.sock");
    let (mut store, instance) = build_instance(path).await;

    let vh = resolve(&mut store, &instance, OUTLET_NAME).await;
    assert_eq!(vh, -1, "resolve with server down must return -1");
}

#[tokio::test]
async fn server_down_list_len_returns_estale() {
    // list_len's failure has exactly one meaning — the server is unreachable
    // (or predates outlets) — so it reports ESTALE rather than the ambiguous -1.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("no_server.sock");
    let (mut store, instance) = build_instance(path).await;

    let n = instance
        .get_typed_func::<(), i32>(&mut store, "outlet_list_len")
        .expect("get outlet_list_len")
        .call_async(&mut store, ())
        .await
        .expect("call outlet_list_len");
    assert_eq!(n, ESTALE, "list_len with server down must return ESTALE");
}

/// swarm#1315: `outlet_type_id` reports the declared command type for a valid
/// handle and -1 for a never-issued one.
#[tokio::test]
async fn outlet_type_id_round_trip_and_errors() {
    let dir = TempDir::new().unwrap();
    let (mut store, instance) = build_instance(spawn_server(&dir)).await;

    let vh = resolve(&mut store, &instance, OUTLET_NAME).await;
    assert!(vh >= 1);

    let f = instance
        .get_typed_func::<(i32, i32, i32), i32>(&mut store, "outlet_type_id")
        .expect("get outlet_type_id");

    // Valid: 4-byte out buffer at 512.
    let status = f.call_async(&mut store, (vh, 512, 4)).await.expect("call");
    assert_eq!(status, 0);
    assert_eq!(
        read_i32(&mut store, &instance, 512).cast_unsigned(),
        OUTLET_TYPE_ID
    );

    // Wrong out length → EINVAL (-22), nothing written.
    let status = f.call_async(&mut store, (vh, 512, 2)).await.expect("call");
    assert_eq!(status, -22, "a non-4-byte out buffer must return EINVAL");

    let status = f
        .call_async(&mut store, (9999, 512, 4))
        .await
        .expect("call");
    assert_eq!(status, -1, "a never-issued handle must return -1");
}
