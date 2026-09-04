//! CI end-to-end + ABI conformance tests for the Linux Signal Layer.
// The Wasmtime WASM host-function interface requires all params/returns to be
// i32; the casts from usize (name len, byte counts) and back are deliberate and
// checked at runtime by the WAT module or host-function implementation.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
//!
//! SR-Trace: SR-10(a)(b), SR-14, SR-16(a), SR-2.
//!
//! ## Step 1 — end-to-end
//!
//! A stub [`TapStore`] whose retained tap is computed through the real,
//! unmodified [`moving_average::MovingAverageState`] step (SR-2), plus a
//! `_signal_layer_health` event tap emitting a postcard-encoded
//! [`HealthEvent`]`{ state: Down }` (SR-14).  The store is served by
//! `run_tap_server` (SR-16a: socket mode 0660).  A WAT-module "cell" loaded
//! via `sorg_execution::wasm_tap::link_tap_functions` (SR-10a) resolves
//! both taps, reads the retained value, and takes the health event.
//!
//! ## Step 2 — ABI conformance
//!
//! `fixtures/tap_probe_wamr_abi.wasm` is compiled from `fixtures/tap_probe_cell.wat`
//! (see that file for the exact build command).  It imports the six tap host
//! functions with the WAMR-identical signatures (`"tap"` module namespace,
//! all i32 params/returns), matching what an ESP32-compiled cell binary
//! expects at import resolution.  The binary is loaded byte-identical — no
//! recompile — into sorg-execution via `link_tap_functions`, and
//! resolve+read must succeed (SR-10b).

use std::{os::unix::fs::MetadataExt as _, path::PathBuf, sync::Arc, time::Duration};

use moving_average::{MovingAverageConfig, MovingAverageState};
use signal_layer_core::ProcessingStep as _;
use signal_layer_ipc::{StoreRead, TapStore};
use signal_layer_types::{DriverHealth, HealthEvent};
use tempfile::TempDir;
use tokio::net::UnixListener;
use wasmtime::{Config, Engine, Instance, Linker, Module, Store};

// ── Moving-average step exercise (SR-2) ──────────────────────────────────────

/// Feed the real, unmodified moving-average step with enough samples to produce
/// an output, then return the serialised retained value bytes.
///
/// The step crate is imported unmodified — this is SR-2's checkable property.
fn compute_retained_via_moving_average() -> Vec<u8> {
    let mut state = MovingAverageState::new(MovingAverageConfig { window: 3 });
    // Need `window` samples to produce an output.
    assert!(state.step(10.0).is_none(), "window not full yet");
    assert!(state.step(20.0).is_none(), "window not full yet");
    let avg = state.step(30.0).expect("window full after 3 samples");
    // avg = (10 + 20 + 30) / 3 = 20.0
    assert!((avg - 20.0).abs() < 1e-5, "moving-average value unexpected");

    postcard::to_allocvec(&avg).expect("postcard serialize f32")
}

// ── Stub TapStore ─────────────────────────────────────────────────────────────

/// Two-tap store: `"temperature"` (retained, filled via moving-average) +
/// `"_signal_layer_health"` (event, one pre-queued [`HealthEvent`] with `Down` state).
struct E2eStore {
    retained_bytes: Vec<u8>,
    health_event_bytes: Vec<u8>,
    health_taken: std::sync::Mutex<bool>,
}

impl E2eStore {
    fn new() -> Self {
        let retained_bytes = compute_retained_via_moving_average();

        let event = HealthEvent {
            source: 0,
            state: DriverHealth::Down,
        };
        let health_event_bytes = postcard::to_allocvec(&event).expect("postcard HealthEvent");

        Self {
            retained_bytes,
            health_event_bytes,
            health_taken: std::sync::Mutex::new(false),
        }
    }
}

impl TapStore for E2eStore {
    fn resolve(&self, name: &str) -> Option<u32> {
        match name {
            "temperature" => Some(1),
            "_signal_layer_health" => Some(2),
            _ => None,
        }
    }

    fn read_retained(&self, h: u32) -> StoreRead {
        match h {
            1 => StoreRead::Value {
                timestamp_ms: 1000,
                bytes: self.retained_bytes.clone(),
            },
            _ => StoreRead::InvalidHandle,
        }
    }

    fn take_event(&self, h: u32) -> StoreRead {
        match h {
            2 => {
                let mut taken = self.health_taken.lock().unwrap();
                if *taken {
                    StoreRead::Empty
                } else {
                    *taken = true;
                    StoreRead::Value {
                        timestamp_ms: 0,
                        bytes: self.health_event_bytes.clone(),
                    }
                }
            }
            _ => StoreRead::InvalidHandle,
        }
    }

    fn list_len(&self) -> u32 {
        2
    }

    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        match index {
            0 => Some(("temperature".to_owned(), 0)), // retained
            1 => Some(("_signal_layer_health".to_owned(), 1)), // event
            _ => None,
        }
    }
}

// ── Wasmtime harness ─────────────────────────────────────────────────────────

/// WAT for the e2e "cell": imports and re-exports all six tap host functions.
/// This is the "existing, unmodified tap-reading test cell" pattern used in
/// sorg-execution's own `tap_host_functions.rs` — SR-10(a).
const TAP_PROBE_WAT: &str = r#"
(module
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

  (memory (export "memory") 1)

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
)
"#;

/// Committed fixture binary compiled from `fixtures/tap_probe_cell.wat`
/// (build: `wasm-tools parse tap_probe_cell.wat -o tap_probe_wamr_abi.wasm`).
const ABI_FIXTURE_WASM: &[u8] = include_bytes!("../fixtures/tap_probe_wamr_abi.wasm");

fn build_wasmtime_env(
    tap_client: Arc<signal_layer_ipc::TapClient>,
) -> (Engine, Linker<()>, Store<()>) {
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config).expect("wasmtime engine");

    let mut linker: Linker<()> = Linker::new(&engine);
    sorg_execution::wasm_tap::link_tap_functions(&mut linker, tap_client)
        .expect("link_tap_functions");

    let mut store = Store::new(&engine, ());
    store.set_fuel(u64::MAX).expect("set fuel");

    (engine, linker, store)
}

async fn make_instance(linker: &Linker<()>, store: &mut Store<()>, engine: &Engine) -> Instance {
    let module = Module::new(engine, TAP_PROBE_WAT).expect("WAT module");
    linker
        .instantiate_async(store, &module)
        .await
        .expect("instantiate")
}

async fn make_instance_from_bytes(
    linker: &Linker<()>,
    store: &mut Store<()>,
    engine: &Engine,
    bytes: &[u8],
) -> Instance {
    let module = Module::new(engine, bytes).expect("binary module");
    linker
        .instantiate_async(store, &module)
        .await
        .expect("instantiate binary module")
}

fn write_str(store: &mut Store<()>, instance: &Instance, offset: usize, s: &str) {
    let mem = instance.get_memory(&mut *store, "memory").expect("memory");
    mem.write(&mut *store, offset, s.as_bytes())
        .expect("write str");
}

fn read_bytes(store: &mut Store<()>, instance: &Instance, offset: usize, n: usize) -> Vec<u8> {
    let mem = instance.get_memory(&mut *store, "memory").expect("memory");
    let mut buf = vec![0u8; n];
    mem.read(&*store, offset, &mut buf).expect("read bytes");
    buf
}

fn read_i64(store: &mut Store<()>, instance: &Instance, offset: usize) -> i64 {
    let b = read_bytes(store, instance, offset, 8);
    i64::from_le_bytes(b.try_into().unwrap())
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

// ── Socket permissions helper ─────────────────────────────────────────────────

fn assert_socket_mode_0660(path: &std::path::Path) {
    let meta = std::fs::metadata(path).expect("socket metadata");
    let mode = meta.mode() & 0o777;
    assert_eq!(mode, 0o660, "socket mode should be 0660, got {mode:o}");
}

fn spawn_server(socket_path: PathBuf, store: Arc<dyn TapStore>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        signal_layer_linux_rt::run_tap_server(socket_path, store)
            .await
            .expect("run_tap_server");
    })
}

// ── Step 1: End-to-end tests ──────────────────────────────────────────────────

/// SR-16(a): `run_tap_server` creates the socket with mode 0660.
#[tokio::test]
async fn e2e_socket_mode_is_0660() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("sl.sock");

    let server = spawn_server(socket_path.clone(), Arc::new(E2eStore::new()));
    // Allow time for the server to bind.
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_socket_mode_0660(&socket_path); // SR-16(a)

    server.abort();
}

/// SR-2: The retained value the cell sees is the output of the real, unmodified
/// moving-average step (window=3, inputs 10/20/30 → avg 20.0 encoded as f32).
#[tokio::test]
async fn e2e_cell_reads_retained_value_from_moving_average_step() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("sl.sock");

    let server = spawn_server(socket_path.clone(), Arc::new(E2eStore::new()));
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = Arc::new(signal_layer_ipc::TapClient::new(socket_path.clone()));
    let (engine, linker, mut store) = build_wasmtime_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    // Resolve "temperature" → handle.
    let name = "temperature";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 =
        call_tap!(&mut store, &instance, "tap_resolve", (0i32, name.len() as i32) -> i32);
    assert!(
        handle >= 1,
        "tap_resolve should return a valid handle, got {handle}"
    );

    // Read retained → should be postcard(20.0_f32).
    let n: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (handle, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert!(
        n > 0,
        "tap_read_retained should return bytes written, got {n}"
    );

    let raw = read_bytes(&mut store, &instance, 256, n as usize);
    let value: f32 = postcard::from_bytes(&raw).expect("decode f32");
    assert!(
        (value - 20.0_f32).abs() < 1e-5,
        "expected moving-average output 20.0, got {value}"
    );

    // Also verify the timestamp was written (SR-14 — timestamp is non-zero).
    let ts = read_i64(&mut store, &instance, 512);
    assert!(ts > 0, "timestamp_ms should be positive, got {ts}");

    server.abort();
}

/// SR-14: A `HealthEvent { state: Down }` emitted on `_signal_layer_health`
/// reaches the cell via `tap_take_event`.
#[tokio::test]
async fn e2e_cell_observes_health_event_down() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("sl2.sock");

    let server = spawn_server(socket_path.clone(), Arc::new(E2eStore::new()));
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = Arc::new(signal_layer_ipc::TapClient::new(socket_path.clone()));
    let (engine, linker, mut store) = build_wasmtime_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    // Resolve "_signal_layer_health" → handle.
    let name = "_signal_layer_health";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 =
        call_tap!(&mut store, &instance, "tap_resolve", (0i32, name.len() as i32) -> i32);
    assert!(
        handle >= 1,
        "health tap handle should be >= 1, got {handle}"
    );

    // Take the health event.
    let n: i32 = call_tap!(&mut store, &instance, "tap_take_event", (handle, 256i32, 64i32) -> i32);
    assert!(n > 0, "tap_take_event should return event bytes, got {n}");

    // Decode and verify it's HealthEvent { state: Down }.
    let raw = read_bytes(&mut store, &instance, 256, n as usize);
    let event: HealthEvent = postcard::from_bytes(&raw).expect("decode HealthEvent");
    assert_eq!(event.source, 0, "expected source=0");
    assert_eq!(
        event.state,
        DriverHealth::Down,
        "expected DriverHealth::Down"
    );

    // Second take should be empty (event consumed).
    let n2: i32 =
        call_tap!(&mut store, &instance, "tap_take_event", (handle, 256i32, 64i32) -> i32);
    assert_eq!(n2, 0, "second tap_take_event should return 0 (empty)");

    server.abort();
}

/// Comprehensive e2e: both retained value AND health event in a single test run,
/// verifying socket mode 0660, retained read with moving-average value, and
/// health event decode — all together per the plan Step 1 requirement.
#[tokio::test]
async fn e2e_full_pipeline_retained_and_health_event_with_socket_mode() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("sl_full.sock");

    let server = spawn_server(socket_path.clone(), Arc::new(E2eStore::new()));
    tokio::time::sleep(Duration::from_millis(50)).await;

    // SR-16(a): socket mode 0660.
    assert_socket_mode_0660(&socket_path);

    let client = Arc::new(signal_layer_ipc::TapClient::new(socket_path.clone()));
    let (engine, linker, mut store) = build_wasmtime_env(client);
    let instance = make_instance(&linker, &mut store, &engine).await;

    // ── Retained tap via moving-average step (SR-2) ──────────────────────────
    let temp_name = "temperature";
    write_str(&mut store, &instance, 0, temp_name);
    let temp_handle: i32 =
        call_tap!(&mut store, &instance, "tap_resolve", (0i32, temp_name.len() as i32) -> i32);
    assert!(temp_handle >= 1);

    let n: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (temp_handle, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert!(n > 0);
    let raw = read_bytes(&mut store, &instance, 256, n as usize);
    let value: f32 = postcard::from_bytes(&raw).expect("decode f32");
    assert!(
        (value - 20.0_f32).abs() < 1e-5,
        "moving-average value should be 20.0"
    );

    // ── Health event tap (SR-14) ─────────────────────────────────────────────
    let health_name = "_signal_layer_health";
    write_str(&mut store, &instance, 0, health_name);
    let health_handle: i32 = call_tap!(
        &mut store, &instance, "tap_resolve",
        (0i32, health_name.len() as i32) -> i32
    );
    assert!(health_handle >= 1);

    let m: i32 =
        call_tap!(&mut store, &instance, "tap_take_event", (health_handle, 256i32, 64i32) -> i32);
    assert!(m > 0, "expected health event bytes");
    let raw_evt = read_bytes(&mut store, &instance, 256, m as usize);
    let event: HealthEvent = postcard::from_bytes(&raw_evt).expect("decode HealthEvent");
    assert_eq!(event.state, DriverHealth::Down);

    server.abort();
}

// ── Step 2: ABI conformance tests ─────────────────────────────────────────────

/// SR-10(b): Load the committed `tap_probe_wamr_abi.wasm` binary byte-identical
/// — no recompile — and assert that `tap_resolve` + `tap_read_retained` work via
/// `sorg_execution::wasm_tap::link_tap_functions`.
///
/// The fixture was compiled from `fixtures/tap_probe_cell.wat` with:
/// ```text
/// wasm-tools parse tap_probe_cell.wat -o tap_probe_wamr_abi.wasm
/// ```
#[tokio::test]
async fn abi_conformance_wasm_fixture_resolve_and_read_work() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("abi.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind");
    tokio::spawn(signal_layer_ipc::serve(listener, Arc::new(E2eStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(socket_path));
    let (engine, linker, mut store) = build_wasmtime_env(client);

    // Load the committed binary (byte-identical, no recompile — SR-10b).
    let instance = make_instance_from_bytes(&linker, &mut store, &engine, ABI_FIXTURE_WASM).await;

    // Resolve "temperature" → valid handle.
    let name = "temperature";
    write_str(&mut store, &instance, 0, name);
    let handle: i32 =
        call_tap!(&mut store, &instance, "tap_resolve", (0i32, name.len() as i32) -> i32);
    assert!(
        handle >= 1,
        "ABI fixture: tap_resolve should return >= 1, got {handle}"
    );

    // Read retained → bytes written > 0.
    let n: i32 = call_tap!(
        &mut store, &instance, "tap_read_retained",
        (handle, 256i32, 64i32, 512i32, 8i32) -> i32
    );
    assert!(
        n > 0,
        "ABI fixture: tap_read_retained should return bytes written, got {n}"
    );

    // Decode and verify value.
    let raw = read_bytes(&mut store, &instance, 256, n as usize);
    let value: f32 = postcard::from_bytes(&raw).expect("decode f32 from ABI fixture");
    assert!(
        (value - 20.0_f32).abs() < 1e-5,
        "ABI fixture: expected 20.0, got {value}"
    );
}

/// SR-10(b): All six tap host functions work with the committed `.wasm` binary.
#[tokio::test]
async fn abi_conformance_all_six_tap_functions_work() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("abi_all.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind");
    tokio::spawn(signal_layer_ipc::serve(listener, Arc::new(E2eStore::new())));

    let client = Arc::new(signal_layer_ipc::TapClient::new(socket_path));
    let (engine, linker, mut store) = build_wasmtime_env(client);
    let instance = make_instance_from_bytes(&linker, &mut store, &engine, ABI_FIXTURE_WASM).await;

    // tap_list_len → 2 taps.
    let count: i32 = call_tap!(&mut store, &instance, "tap_list_len", () -> i32);
    assert_eq!(count, 2, "ABI fixture: tap_list_len should return 2");

    // tap_list_entry(0) → "temperature", kind=0 (retained).
    let n0: i32 = call_tap!(
        &mut store, &instance, "tap_list_entry",
        (0i32, 256i32, 64i32, 512i32, 4i32) -> i32
    );
    assert_eq!(
        n0,
        "temperature".len() as i32,
        "ABI fixture: tap_list_entry 0 should return name len"
    );

    // tap_resolve + tap_take_event on health tap.
    let health = "_signal_layer_health";
    write_str(&mut store, &instance, 0, health);
    let hh: i32 =
        call_tap!(&mut store, &instance, "tap_resolve", (0i32, health.len() as i32) -> i32);
    assert!(hh >= 1);

    let m: i32 = call_tap!(&mut store, &instance, "tap_take_event", (hh, 256i32, 64i32) -> i32);
    assert!(
        m > 0,
        "ABI fixture: tap_take_event should return event bytes"
    );

    // tap_drain_batch → always 0 (D1).
    let db: i32 = call_tap!(&mut store, &instance, "tap_drain_batch", (hh, 256i32, 64i32) -> i32);
    assert_eq!(db, 0, "ABI fixture: tap_drain_batch should always return 0");
}
