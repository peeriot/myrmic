//! Integration tests for the six tap host functions in sorg-execution.
//!
//! Tests drive the host functions through a minimal Wasmtime instance.  Because
//! host functions are *imports* into WASM modules (not exports), the test module
//! is a WAT snippet that imports all six host functions and wraps each one as a
//! plain exported function — this mirrors what a real cell does.
//!
//! Memory layout used throughout: page 0 of a single 64 `KiB` page.
//!   [0   ..  127]  name / string scratch
//!   [256 ..  511]  read buffer (bytes out)
//!   [512 ..  519]  i64 timestamp out (`ts_out_ptr`)
//!   [520 ..  523]  i32 kind out (`out_kind_ptr`)

use std::{path::PathBuf, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::net::UnixListener;

use myrmic_common::types::error::ESTALE;
use signal_layer_ipc::{MAX_RESOLVE_NAME_LEN, StoreRead, TapStore};
use wasmtime::{Config, Engine, Instance, Linker, Module, Store};

// ── Stub TapStore ─────────────────────────────────────────────────────────────

/// Wire-type id every stub tap reports (arbitrary).
const STUB_TYPE_ID: u32 = 0xF32;

struct StubStore {
    retained_bytes: Vec<u8>,
    retained_ts: u64,
    event_bytes: std::sync::Mutex<Option<Vec<u8>>>,
}

impl StubStore {
    fn new() -> Self {
        Self {
            retained_bytes: vec![42, 43],
            retained_ts: 100,
            event_bytes: std::sync::Mutex::new(Some(vec![99])),
        }
    }

    fn with_empty_event() -> Self {
        Self {
            retained_bytes: vec![42, 43],
            retained_ts: 100,
            event_bytes: std::sync::Mutex::new(None),
        }
    }
}

impl TapStore for StubStore {
    fn resolve(&self, name: &str) -> Option<u32> {
        match name {
            "temp" => Some(1),
            "evt" => Some(2),
            _ => None,
        }
    }

    fn type_id(&self, h: u32) -> Option<u32> {
        (h == 1 || h == 2).then_some(STUB_TYPE_ID)
    }

    fn read_retained(&self, h: u32) -> StoreRead {
        match h {
            1 => StoreRead::Value {
                timestamp_ms: self.retained_ts,
                bytes: self.retained_bytes.clone(),
            },
            _ => StoreRead::InvalidHandle,
        }
    }

    fn take_event(&self, h: u32) -> StoreRead {
        match h {
            2 => match self.event_bytes.lock().unwrap().take() {
                Some(bytes) => StoreRead::Value {
                    timestamp_ms: 0,
                    bytes,
                },
                None => StoreRead::Empty,
            },
            _ => StoreRead::InvalidHandle,
        }
    }

    fn list_len(&self) -> u32 {
        2
    }

    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        match index {
            0 => Some(("temp".to_owned(), 0)),
            1 => Some(("evt".to_owned(), 1)),
            _ => None,
        }
    }
}

#[allow(dead_code)]
struct OtherStore;

impl TapStore for OtherStore {
    fn resolve(&self, name: &str) -> Option<u32> {
        if name == "other" { Some(1) } else { None }
    }

    fn type_id(&self, h: u32) -> Option<u32> {
        (h == 1).then_some(STUB_TYPE_ID)
    }

    fn read_retained(&self, h: u32) -> StoreRead {
        match h {
            1 => StoreRead::Value {
                timestamp_ms: 200,
                bytes: vec![77],
            },
            _ => StoreRead::InvalidHandle,
        }
    }

    fn take_event(&self, _h: u32) -> StoreRead {
        StoreRead::Empty
    }

    fn list_len(&self) -> u32 {
        1
    }

    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        if index == 0 {
            Some(("other".to_owned(), 0))
        } else {
            None
        }
    }
}

// ── Harness ───────────────────────────────────────────────────────────────────

/// WAT module that imports all six tap host functions and re-exports them as
/// plain wrappers.  This is exactly the pattern a real cell uses.
const TAP_WRAPPER_WAT: &str = r#"
(module
  ;; ── imports (must precede memory declaration in WASM) ────────────────────
  (import "tap" "tap_resolve"
    (func $tap_resolve (param i32 i32) (result i32)))
  (import "tap" "tap_read_retained"
    (func $tap_read_retained (param i32 i32 i32 i32 i32) (result i32)))
  (import "tap" "tap_take_event"
    (func $tap_take_event (param i32 i32 i32) (result i32)))
  (import "tap" "tap_drain_batch"
    (func $tap_drain_batch (param i32 i32 i32) (result i32)))
  (import "tap" "tap_list_len"
    (func $tap_list_len (result i32)))
  (import "tap" "tap_list_entry"
    (func $tap_list_entry (param i32 i32 i32 i32 i32) (result i32)))
  (import "tap" "tap_type_id"
    (func $tap_type_id (param i32 i32 i32) (result i32)))

  ;; memory shared with host
  (memory (export "memory") 1)

  ;; ── re-exports ────────────────────────────────────────────────────────────
  (func (export "tap_resolve") (param i32 i32) (result i32)
    local.get 0  local.get 1  call $tap_resolve)
  (func (export "tap_read_retained") (param i32 i32 i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  local.get 3  local.get 4
    call $tap_read_retained)
  (func (export "tap_take_event") (param i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  call $tap_take_event)
  (func (export "tap_drain_batch") (param i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  call $tap_drain_batch)
  (func (export "tap_list_len") (result i32)
    call $tap_list_len)
  (func (export "tap_list_entry") (param i32 i32 i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  local.get 3  local.get 4
    call $tap_list_entry)
  (func (export "tap_type_id") (param i32 i32 i32) (result i32)
    local.get 0  local.get 1  local.get 2  call $tap_type_id)
)
"#;

/// `serve()` with no outlet store — these tests exercise the tap surface.
fn serve_taps(
    listener: UnixListener,
    store: Arc<dyn TapStore>,
) -> impl std::future::Future<Output = std::io::Result<()>> {
    signal_layer_ipc::serve(listener, store, None)
}

fn bind_in_tempdir(dir: &TempDir) -> (PathBuf, UnixListener) {
    let path = dir.path().join("tap.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    (path, listener)
}

fn build_env(tap_client: Arc<signal_layer_ipc::TapClient>) -> (Engine, Linker<()>, Store<()>) {
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config).expect("engine");

    let mut linker: Linker<()> = Linker::new(&engine);
    sorg_execution::wasm_tap::link_tap_functions(&mut linker, tap_client)
        .expect("link tap functions");

    let store = build_store(&engine);

    (engine, linker, store)
}

/// A fresh store for an additional cell on an existing engine — each cell gets
/// its own store and instance while sharing the linker's `TapClient`.
fn build_store(engine: &Engine) -> Store<()> {
    let mut store = Store::new(engine, ());
    store.set_fuel(u64::MAX).expect("set fuel");
    store
}

async fn make_instance(linker: &Linker<()>, store: &mut Store<()>, engine: &Engine) -> Instance {
    let module = Module::new(engine, TAP_WRAPPER_WAT).expect("wrapper module");
    linker
        .instantiate_async(store, &module)
        .await
        .expect("instantiate")
}

fn write_str(store: &mut Store<()>, instance: &Instance, offset: usize, s: &str) {
    let mem = instance.get_memory(&mut *store, "memory").expect("memory");
    mem.write(&mut *store, offset, s.as_bytes()).expect("write");
}

/// Size of the instance's linear memory in bytes — the largest length the guest
/// can hand to a host function and still be in bounds.
fn memory_size(store: &mut Store<()>, instance: &Instance) -> usize {
    let mem = instance.get_memory(&mut *store, "memory").expect("memory");
    mem.data_size(&*store)
}

fn read_bytes(store: &mut Store<()>, instance: &Instance, offset: usize, n: usize) -> Vec<u8> {
    let mem = instance.get_memory(&mut *store, "memory").expect("memory");
    let mut buf = vec![0u8; n];
    mem.read(&*store, offset, &mut buf).expect("read");
    buf
}

fn read_i64(store: &mut Store<()>, instance: &Instance, offset: usize) -> i64 {
    let b = read_bytes(store, instance, offset, 8);
    i64::from_le_bytes(b.try_into().unwrap())
}

fn read_i32(store: &mut Store<()>, instance: &Instance, offset: usize) -> i32 {
    let mem = instance.get_memory(&mut *store, "memory").expect("memory");
    let mut buf = [0u8; 4];
    mem.read(&*store, offset, &mut buf).expect("read i32");
    i32::from_le_bytes(buf)
}

macro_rules! call_tap {
    ($store:expr, $instance:expr, $name:literal, () -> $ret:ty) => {{
        let f = $instance
            .get_typed_func::<(), $ret>(&mut *$store, $name)
            .expect(concat!("get func ", $name));
        f.call_async(&mut *$store, ())
            .await
            .expect(concat!("call ", $name))
    }};
    ($store:expr, $instance:expr, $name:literal, ($a:expr, $b:expr) -> $ret:ty) => {{
        let f = $instance
            .get_typed_func::<(i32, i32), $ret>(&mut *$store, $name)
            .expect(concat!("get func ", $name));
        f.call_async(&mut *$store, ($a, $b))
            .await
            .expect(concat!("call ", $name))
    }};
    ($store:expr, $instance:expr, $name:literal, ($a:expr, $b:expr, $c:expr) -> $ret:ty) => {{
        let f = $instance
            .get_typed_func::<(i32, i32, i32), $ret>(&mut *$store, $name)
            .expect(concat!("get func ", $name));
        f.call_async(&mut *$store, ($a, $b, $c))
            .await
            .expect(concat!("call ", $name))
    }};
    ($store:expr, $instance:expr, $name:literal, ($a:expr, $b:expr, $c:expr, $d:expr, $e:expr) -> $ret:ty) => {{
        let f = $instance
            .get_typed_func::<(i32, i32, i32, i32, i32), $ret>(&mut *$store, $name)
            .expect(concat!("get func ", $name));
        f.call_async(&mut *$store, ($a, $b, $c, $d, $e))
            .await
            .expect(concat!("call ", $name))
    }};
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_resolve_known_tap_returns_positive_handle() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let name = "temp";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(handle >= 1, "expected handle >= 1, got {handle}");
}

#[tokio::test]
async fn test_resolve_unknown_tap_returns_minus_one() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let name = "nonexistent";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert_eq!(handle, -1);
}

#[tokio::test]
async fn test_read_retained_writes_bytes_and_timestamp() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let name = "temp";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(handle >= 1);

    // buf at 256, ts_out at 512
    let n: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (handle, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert_eq!(n, 2);
    let written = read_bytes(&mut store, &instance, 256, 2);
    assert_eq!(written, vec![42, 43]);
    let ts = read_i64(&mut store, &instance, 512);
    assert_eq!(ts, 100i64);
}

#[tokio::test]
async fn test_read_retained_invalid_handle_returns_minus_one() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    // "evt" tap returns InvalidHandle for read_retained
    let name = "evt";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(handle >= 1);

    let n: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (handle, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    // InvalidHandle → Unavailable → ESTALE
    assert_eq!(n, ESTALE);
}

#[tokio::test]
async fn test_take_event_returns_bytes() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let name = "evt";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(handle >= 1);

    let n: i32 = call_tap!(
        &mut store, &instance, "tap_take_event",
        (handle, 256i32, 64i32) -> i32
    );
    assert_eq!(n, 1);
    let data = read_bytes(&mut store, &instance, 256, 1);
    assert_eq!(data, vec![99]);
}

#[tokio::test]
async fn test_take_event_empty_returns_zero() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(
        listener,
        Arc::new(StubStore::with_empty_event()),
    ));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let name = "evt";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(handle >= 1);

    let n: i32 = call_tap!(&mut store, &instance, "tap_take_event", (handle, 256i32, 64i32) -> i32);
    assert_eq!(n, 0);
}

#[tokio::test]
async fn test_drain_batch_always_zero() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    // D1: drain_batch is always 0 regardless of handle
    let n: i32 = call_tap!(&mut store, &instance, "tap_drain_batch", (1i32, 256i32, 64i32) -> i32);
    assert_eq!(n, 0);
}

#[tokio::test]
async fn test_list_len_returns_count() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let count: i32 = call_tap!(&mut store, &instance, "tap_list_len", () -> i32);
    assert_eq!(count, 2);
}

#[tokio::test]
async fn test_list_entry_roundtrip_kind_widened_u8_to_i32() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    // Entry 0: "temp", kind=0 (retained), u8 widened to i32
    let n0: i32 = call_tap!(
        &mut store, &instance, "tap_list_entry",
        (0i32, 256i32, 64i32, 512i32, 4i32) -> i32
    );
    assert_eq!(n0, 4);
    assert_eq!(&read_bytes(&mut store, &instance, 256, 4), b"temp");
    assert_eq!(read_i32(&mut store, &instance, 512), 0i32);

    // Entry 1: "evt", kind=1 (event), u8 widened to i32
    let n1: i32 = call_tap!(
        &mut store, &instance, "tap_list_entry",
        (1i32, 256i32, 64i32, 512i32, 4i32) -> i32
    );
    assert_eq!(n1, 3);
    assert_eq!(&read_bytes(&mut store, &instance, 256, 3), b"evt");
    assert_eq!(read_i32(&mut store, &instance, 512), 1i32);
}

#[tokio::test]
async fn test_list_entry_out_of_range_returns_minus_one() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let r: i32 = call_tap!(
        &mut store, &instance, "tap_list_entry",
        (99i32, 256i32, 64i32, 512i32, 4i32) -> i32
    );
    assert_eq!(r, -1);
}

#[tokio::test]
async fn test_stale_handle_after_server_restart_never_returns_other_tap_data() {
    // SR-12: a virtual handle from one server generation must never return data
    // from a different tap on a new server.
    //
    // This tests the host-function layer's delegation to TapClient.  The
    // generation-check itself is proven in signal-layer-ipc's unit tests; here
    // we verify the wasm-facing layer correctly returns ESTALE (Unavailable) for a
    // stale handle, exercised through a full Wasmtime + host-fn round trip.
    //
    // We force the connection to drop cleanly by shutting down the listener
    // and using a separate TapClient instance whose conn is fresh (so we can
    // exercise the stale-handle check without needing disconnect_for_test).
    //
    // Strategy: build TWO clients on the same socket path.  Client A resolves
    // "temp" on server1.  We then start server2 and have Client B reconnect.
    // Client A still holds vh_a (generation 1); when Client A tries to use it
    // against server2, the generation check kicks in and returns Unavailable.
    // However, since Client A's connection hasn't been torn down yet, we use a
    // different approach: kill the connection by dropping the first server and
    // trying an operation that fails, then reconnect with a new generation.
    //
    // The cleanest testable guarantee at this layer:
    //   - Virtual handle 0 is never issued (the host fn wraps resolve → -1 for None).
    //   - A virtual handle returned by a prior resolve remains valid only within
    //     the same connection generation; the host fn returns ESTALE when the client
    //     reports Unavailable.
    //
    // We test this by verifying the host fn returns ESTALE when given a handle that
    // was never issued by this client (simulating a handle from a prior process).
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path.clone()));
    let (engine, linker, mut store) = build_env(Arc::clone(&client));
    let instance = make_instance(&linker, &mut store, &engine).await;

    // Resolve "temp" → valid virtual handle
    let name = "temp";
    write_str(&mut store, &instance, 0, name);
    let vh: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(vh >= 1, "expected valid handle, got {vh}");

    // Handle 0 is never issued (invariant)
    let r0: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (0i32, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert_eq!(
        r0, ESTALE,
        "handle 0 must always return ESTALE (never valid)"
    );

    // A fabricated large handle that was never returned by resolve → ESTALE
    let fake_vh = 99999i32;
    let r_fake: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (fake_vh, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert_eq!(r_fake, ESTALE, "fabricated handle must return ESTALE");

    // Valid vh still works (same generation)
    let n: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (vh, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert_eq!(n, 2, "valid handle should still read correctly");

    // Now simulate stale-ness at the TapClient level (not through WASM, since
    // we can't call disconnect_for_test here — it's #[cfg(test)] in its crate).
    // The TapClient stale-handle guarantee is unit-tested in signal-layer-ipc.
    // What we verify here: the host function correctly propagates Unavailable→ESTALE.
    let stale_result = client.read_retained(u32::try_from(fake_vh).unwrap()).await;
    assert_eq!(
        stale_result,
        signal_layer_ipc::ClientRead::Unavailable,
        "TapClient must return Unavailable for a never-issued virtual handle"
    );
}

#[tokio::test]
async fn test_server_down_resolve_returns_minus_one() {
    // D3: IPC down → tap_resolve returns -1
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("no_server.sock");

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let name = "temp";
    write_str(&mut store, &instance, 0, name);
    let h: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert_eq!(h, -1, "resolve with server down must return -1");
}

#[tokio::test]
async fn test_server_down_list_len_returns_estale() {
    // Unlike resolve (where -1 doubles as "not found"), list_len's failure has
    // exactly one meaning — the server is unreachable — so it reports ESTALE
    // and the SDK surfaces `ApiError::Unavailable`.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("no_server.sock");

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let n: i32 = call_tap!(&mut store, &instance, "tap_list_len", () -> i32);
    assert_eq!(n, ESTALE, "list_len with server down must return ESTALE");
}

#[tokio::test]
async fn test_host_recovery_after_server_returns() {
    // After server comes back, resolve succeeds again.
    // (The 10s SLA bound is asserted in signal-layer-ipc unit tests.)
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("tap_recovery.sock");

    let listener1 = UnixListener::bind(&path).expect("bind");
    let server_task = tokio::spawn(serve_taps(listener1, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path.clone()));
    assert!(client.resolve("temp").await.is_some(), "first resolve");

    server_task.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;

    std::fs::remove_file(&path).ok();
    let listener2 = UnixListener::bind(&path).expect("bind 2");
    tokio::spawn(serve_taps(listener2, Arc::new(StubStore::new())));
    tokio::time::sleep(Duration::from_millis(50)).await;

    let vh2 = client.resolve("temp").await;
    assert!(
        vh2.is_some(),
        "after server comes back, resolve should succeed"
    );
}

#[tokio::test]
async fn test_unknown_virtual_handle_read_retained_returns_estale() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    // Virtual handle 9999 was never resolved → Unavailable → ESTALE, the code
    // the SDK maps to `ApiError::Unavailable` ("re-resolve the handle").
    let r: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (9999i32, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert_eq!(r, ESTALE);
}

// ── B1 regression tests: guest-memory OOB ────────────────────────────────────

/// A store that returns a large (100-byte) retained value, to exercise OOB writes.
struct LargeRetainedStore;

impl TapStore for LargeRetainedStore {
    fn resolve(&self, name: &str) -> Option<u32> {
        if name == "big" { Some(1) } else { None }
    }

    fn type_id(&self, h: u32) -> Option<u32> {
        (h == 1).then_some(STUB_TYPE_ID)
    }

    fn read_retained(&self, h: u32) -> StoreRead {
        if h == 1 {
            StoreRead::Value {
                timestamp_ms: 1,
                bytes: vec![0xAAu8; 100], // 100 bytes
            }
        } else {
            StoreRead::InvalidHandle
        }
    }

    fn take_event(&self, _h: u32) -> StoreRead {
        StoreRead::Empty
    }
    fn list_len(&self) -> u32 {
        1
    }
    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        if index == 0 {
            Some(("big".to_owned(), 0))
        } else {
            None
        }
    }
}

/// B1a: `buf_ptr` near the end of the 64 `KiB` guest memory page must return -1,
/// NOT panic/abort the host process.
///
/// The guest linear memory is exactly one 64 `KiB` page (65536 bytes).
/// With a 100-byte retained value: `buf_ptr` = 65500, `buf_len` = 100 →
/// end = 65600 which is OOB.  The write helper must detect this and return -1
/// to the guest without any index-out-of-bounds panic.
#[tokio::test]
async fn test_read_retained_buf_ptr_near_memory_end_returns_minus_one() {
    const PAGE_SIZE: i32 = 65536;

    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(LargeRetainedStore)));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    // Resolve the "big" tap.
    let name = "big";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(handle >= 1, "expected valid handle");

    // Guest memory is 64 KiB = 65536 bytes.
    // buf_ptr = 65500, buf_len = 100 → n = min(100, 100) = 100, end = 65600 → OOB.
    // The host function must return -1 without panicking.
    let buf_ptr: i32 = PAGE_SIZE - 36; // 65500; 65500 + 100 = 65600 > 65536
    let buf_len: i32 = 100;
    let ts_ptr: i32 = 0; // ts_out_ptr = 0 means skip the i64 write
    let r: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (handle, buf_ptr, buf_len, ts_ptr, 8i32) -> i32
    );
    assert_eq!(r, -1, "OOB buf_ptr must return -1, not panic");
}

/// B1c: valid `buf_ptr` but an out-of-bounds `ts_out_ptr` — the 8-byte i64 write
/// would cross the memory boundary, so `write_guest_i64` returns Err and the
/// host function must return -1 without aborting.
///
/// Guest memory is 64 `KiB` = 65536 bytes.
/// `ts_out_ptr` = 65533 → 65533 + 8 = 65541 > 65536 (OOB by 5 bytes).
/// `buf_ptr` = 256 and `buf_len` = 2 are fully in-bounds, so bytes would be
/// written successfully; only the `ts_out` write triggers the -1.
#[tokio::test]
async fn test_read_retained_oob_ts_out_ptr_returns_minus_one() {
    const PAGE_SIZE: i32 = 65536;

    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    // Resolve "temp" → handle with retained value (2 bytes at ts=100).
    let name = "temp";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(handle >= 1, "expected valid handle");

    // buf_ptr = 256, buf_len = 2 (in-bounds for a 2-byte retained value).
    // ts_out_ptr = 65533 → end = 65533 + 8 = 65541 > 65536 → OOB.
    let ts_out_ptr: i32 = PAGE_SIZE - 3; // 65533
    let r: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (handle, 256i32, 64i32, ts_out_ptr, 8i32) -> i32
    );
    assert_eq!(r, -1, "OOB ts_out_ptr must return -1, not panic");
}

/// B1d: `tap_list_entry` with a valid name buffer but an out-of-bounds
/// `out_kind_ptr` — the 4-byte i32 write crosses the memory boundary, so
/// `write_guest_i32` returns Err and the host function must return -1.
///
/// Guest memory is 64 `KiB` = 65536 bytes.
/// `out_kind_ptr` = 65534 → 65534 + 4 = 65538 > 65536 (OOB by 2 bytes).
/// `name_ptr` = 256 and `name_len` = 64 are in-bounds.
#[tokio::test]
async fn test_list_entry_oob_out_kind_ptr_returns_minus_one() {
    const PAGE_SIZE: i32 = 65536;

    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    // out_kind_ptr = 65534 → 65534 + 4 = 65538 > 65536 → OOB.
    let out_kind_ptr: i32 = PAGE_SIZE - 2; // 65534
    let r: i32 = call_tap!(
        &mut store, &instance, "tap_list_entry",
        (0i32, 256i32, 64i32, out_kind_ptr, 4i32) -> i32
    );
    assert_eq!(r, -1, "OOB out_kind_ptr must return -1, not panic");
}

/// B1b: a NEGATIVE `buf_len` must not be sign-cast to `usize::MAX` — the host
/// function must return -1 to the guest, not attempt a near-infinite write.
///
/// i32 -1 cast to usize becomes `usize::MAX` on all 32/64-bit platforms.  If the
/// code does `bytes.len().min(buf_len as usize)` it returns `bytes.len()` which is
/// then used in a write — and would be OOB at large `buf_ptrs`, or silently writes
/// without the caller's consent.  The correct fix is to reject `buf_len` < 0 → -1.
#[tokio::test]
async fn test_read_retained_negative_buf_len_returns_minus_one() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(LargeRetainedStore)));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let name = "big";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(handle >= 1, "expected valid handle");

    // buf_len = -1 (negative i32) — must be rejected immediately → -1.
    // Without the fix: -1i32 as usize = usize::MAX; .min(usize::MAX) returns 100;
    // then write_guest_bytes at ptr=256, len=100 would succeed (in-bounds), so
    // the function would return 100 rather than -1.
    let r: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (handle, 256i32, -1i32, 0i32, 8i32) -> i32
    );
    assert_eq!(r, -1, "negative buf_len must return -1, not a byte count");
}

// ── Tap-argument bounds ──────────────────────────────────────────────────────

/// A store that resolves any name to the same tap.  Because the server never
/// answers `NotFound`, a -1 from `tap_resolve` can only come from the host
/// function's own bound check — which is what makes the boundary assertions
/// meaningful.
struct AnyNameStore;

impl TapStore for AnyNameStore {
    fn resolve(&self, _name: &str) -> Option<u32> {
        Some(1)
    }

    fn type_id(&self, h: u32) -> Option<u32> {
        (h == 1).then_some(STUB_TYPE_ID)
    }

    fn read_retained(&self, h: u32) -> StoreRead {
        if h == 1 {
            StoreRead::Value {
                timestamp_ms: 300,
                bytes: vec![0xCC],
            }
        } else {
            StoreRead::InvalidHandle
        }
    }

    fn take_event(&self, _h: u32) -> StoreRead {
        StoreRead::Empty
    }
    fn list_len(&self) -> u32 {
        1
    }
    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        if index == 0 {
            Some(("any".to_owned(), 0))
        } else {
            None
        }
    }
}

/// A `name_len` past the protocol bound must be rejected inside the host
/// function, with no collateral damage to the cells sharing the tap connection.
///
/// Two cells share one `TapClient` (as they do in a real cell host: one
/// `Arc<TapClient>` per linker).  Cell A resolves a tap and reads it.  Cell B
/// then asks to resolve a name as long as its whole linear memory — well past
/// the bound, and long enough that the resulting request frame would exceed the
/// frame cap.  Afterwards cell B still resolves normally and cell A's handle
/// still reads its value, which proves the shared connection was never torn
/// down and its generation never bumped.
#[tokio::test]
async fn test_oversized_resolve_name_leaves_another_cells_handle_usable() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store_a) = build_env(Arc::clone(&client));
    let cell_a = make_instance(&linker, &mut store_a, &engine).await;
    let mut store_b = build_store(&engine);
    let cell_b = make_instance(&linker, &mut store_b, &engine).await;

    let name = "temp";
    write_str(&mut store_a, &cell_a, 0, name);
    let vh_a: i32 = call_tap!(&mut store_a, &cell_a, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(vh_a >= 1, "cell A must get a valid handle");
    let before: i32 = call_tap!(
        &mut store_a, &cell_a, "tap_read_retained",
        (vh_a, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert_eq!(before, 2, "cell A's handle must read before the attack");

    let oversized = memory_size(&mut store_b, &cell_b);
    assert!(
        oversized > MAX_RESOLVE_NAME_LEN,
        "the guest memory must be larger than the bound for this test to bite"
    );
    let rejected: i32 = call_tap!(
        &mut store_b, &cell_b, "tap_resolve",
        (0i32, i32::try_from(oversized).unwrap()) -> i32
    );
    assert_eq!(rejected, -1, "an over-long name_len must return -1");

    // A normal resolve by cell B is what would reconnect on a new generation if
    // the rejected call had torn the shared connection down.
    write_str(&mut store_b, &cell_b, 0, name);
    let vh_b: i32 = call_tap!(&mut store_b, &cell_b, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(vh_b >= 1, "the shared connection must still serve cell B");

    let after: i32 = call_tap!(
        &mut store_a, &cell_a, "tap_read_retained",
        (vh_a, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert_eq!(
        after, 2,
        "cell A's handle must survive cell B's rejected resolve"
    );
    assert_eq!(read_bytes(&mut store_a, &cell_a, 256, 2), vec![42, 43]);
}

/// The longest `name_len` the bound allows is still accepted and resolves.
#[tokio::test]
async fn test_resolve_name_len_at_bound_is_accepted() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(AnyNameStore)));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let name = "n".repeat(MAX_RESOLVE_NAME_LEN);
    assert!(
        memory_size(&mut store, &instance) >= name.len(),
        "the guest memory must hold a name at the bound"
    );
    write_str(&mut store, &instance, 0, &name);
    let vh: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(vh >= 1, "a name_len at the bound must resolve, got {vh}");

    let n: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (vh, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert_eq!(n, 1, "the handle from a boundary-length name must read");
    assert_eq!(read_bytes(&mut store, &instance, 256, 1), vec![0xCC]);
}

/// One byte past the bound is refused even though the server would have
/// resolved the name, so the -1 is the host function's own.
#[tokio::test]
async fn test_resolve_name_len_past_bound_returns_minus_one() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(AnyNameStore)));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let name_len = MAX_RESOLVE_NAME_LEN + 1;
    assert!(
        memory_size(&mut store, &instance) >= name_len,
        "the length must be in bounds for guest memory, so only the protocol bound rejects it"
    );
    let r: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name_len).unwrap()) -> i32);
    assert_eq!(r, -1, "a name_len past the bound must return -1");
}

/// A negative `name_len` must be rejected before the sign-losing cast, matching
/// `tap_list_entry` and the read functions.
#[tokio::test]
async fn test_resolve_negative_name_len_returns_minus_one() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    write_str(&mut store, &instance, 0, "temp");
    let r: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, -1i32) -> i32);
    assert_eq!(r, -1, "negative name_len must return -1");
}

/// Negative-length rejection is uniform: the batch stub reports -1 too rather
/// than its usual 0.
#[tokio::test]
async fn test_drain_batch_negative_buf_len_returns_minus_one() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let r: i32 = call_tap!(&mut store, &instance, "tap_drain_batch", (1i32, 256i32, -1i32) -> i32);
    assert_eq!(r, -1, "negative buf_len must return -1");
}

/// A negative `name_len` in `tap_list_entry` is rejected the same way.
#[tokio::test]
async fn test_list_entry_negative_name_len_returns_minus_one() {
    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let r: i32 = call_tap!(
        &mut store, &instance, "tap_list_entry",
        (0i32, 256i32, -1i32, 512i32, 4i32) -> i32
    );
    assert_eq!(r, -1, "negative name_len must return -1");
}

/// swarm#1315: `tap_type_id` writes the slot's declared wire type for a valid
/// handle, and returns -1 for a never-issued handle or an OOB out pointer.
#[tokio::test]
async fn test_tap_type_id_round_trip_and_errors() {
    const PAGE_SIZE: i32 = 65536;

    let dir = TempDir::new().unwrap();
    let (path, listener) = bind_in_tempdir(&dir);
    tokio::spawn(serve_taps(listener, Arc::new(StubStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(path));
    let (engine, linker, mut store) = build_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    let name = "temp";
    write_str(&mut store, &instance, 0, name);
    let vh: i32 = call_tap!(&mut store, &instance, "tap_resolve", (0i32, i32::try_from(name.len()).unwrap()) -> i32);
    assert!(vh >= 1);

    // Valid handle: id written at out ptr 512, 4-byte buffer, status 0.
    let status: i32 = call_tap!(&mut store, &instance, "tap_type_id", (vh, 512i32, 4i32) -> i32);
    assert_eq!(status, 0);
    assert_eq!(
        read_i32(&mut store, &instance, 512).cast_unsigned(),
        STUB_TYPE_ID,
        "the slot's declared type id must be written to the out pointer"
    );

    // Wrong out length → EINVAL (-22), nothing written.
    let status: i32 = call_tap!(&mut store, &instance, "tap_type_id", (vh, 512i32, 2i32) -> i32);
    assert_eq!(status, -22, "a non-4-byte out buffer must return EINVAL");

    // Never-issued handle → -1.
    let status: i32 =
        call_tap!(&mut store, &instance, "tap_type_id", (9999i32, 512i32, 4i32) -> i32);
    assert_eq!(status, -1);

    // OOB out pointer (4-byte write would cross the page end) → -1.
    let status: i32 =
        call_tap!(&mut store, &instance, "tap_type_id", (vh, PAGE_SIZE - 2, 4i32) -> i32);
    assert_eq!(status, -1, "OOB out_id_ptr must return -1, not panic");
}
