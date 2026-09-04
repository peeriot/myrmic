//! Integration-style tests for the IPC server.
//!
//! Each test spins up a real `UnixListener` in a tempdir, starts the server
//! as a background task, and connects a raw client that speaks the wire
//! protocol directly.

#![cfg(test)]

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::framing::{decode_frame, read_frame, write_frame};
use crate::types::{OutletStore, Request, Response, StoreRead, StoreWrite, TapStore};
use crate::{PROTOCOL_VERSION, serve};

// ── Stub TapStore ─────────────────────────────────────────────────────────────

struct StubStore;

/// Wire-type ids the stubs report (arbitrary, distinct).
const TAP_TYPE_ID: u32 = 0xF32;
const OUTLET_TYPE_ID: u32 = 0xD16;

const TEMP_NAME: &str = "temperature";
const TEMP_HANDLE: u32 = 1;
const HUM_NAME: &str = "humidity";
const HUM_HANDLE: u32 = 2;

impl TapStore for StubStore {
    fn resolve(&self, name: &str) -> Option<u32> {
        match name {
            TEMP_NAME => Some(TEMP_HANDLE),
            HUM_NAME => Some(HUM_HANDLE),
            _ => None,
        }
    }

    fn type_id(&self, h: u32) -> Option<u32> {
        (h == TEMP_HANDLE || h == HUM_HANDLE).then_some(TAP_TYPE_ID)
    }

    fn read_retained(&self, h: u32) -> StoreRead {
        match h {
            TEMP_HANDLE => StoreRead::Value {
                timestamp_ms: 42,
                bytes: vec![1, 2, 3],
            },
            HUM_HANDLE => StoreRead::Empty,
            _ => StoreRead::InvalidHandle,
        }
    }

    fn take_event(&self, h: u32) -> StoreRead {
        match h {
            TEMP_HANDLE => StoreRead::Value {
                timestamp_ms: 0,
                bytes: vec![7, 8],
            },
            HUM_HANDLE => StoreRead::Empty,
            _ => StoreRead::InvalidHandle,
        }
    }

    fn list_len(&self) -> u32 {
        2
    }

    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        match index {
            0 => Some((TEMP_NAME.into(), 0)),
            1 => Some((HUM_NAME.into(), 1)),
            _ => None,
        }
    }
}

// ── Helper: bind a server and return the socket path ─────────────────────────

fn bind_server(dir: &tempfile::TempDir) -> (UnixListener, std::path::PathBuf) {
    let path = dir.path().join("test.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    (listener, path)
}

async fn connect_and_handshake(path: &std::path::Path) -> UnixStream {
    let mut stream = UnixStream::connect(path).await.expect("connect");
    // Send Hello
    write_frame(
        &mut stream,
        &Request::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await
    .expect("write Hello");
    // Expect HelloOk
    let frame = read_frame(&mut stream).await.expect("read HelloOk frame");
    let resp: Response = decode_frame(&frame).expect("decode HelloOk");
    assert_eq!(
        resp,
        Response::HelloOk {
            version: PROTOCOL_VERSION
        }
    );
    stream
}

async fn send_request(stream: &mut UnixStream, req: &Request) -> Response {
    write_frame(stream, req).await.expect("write request");
    let frame = read_frame(stream).await.expect("read response frame");
    decode_frame(&frame).expect("decode response")
}

fn spawn_server(listener: UnixListener) {
    let store: Arc<dyn TapStore> = Arc::new(StubStore);
    tokio::spawn(async move {
        let _ = serve(listener, store, None).await;
    });
}

// ── Stub OutletStore ──────────────────────────────────────────────────────────

struct StubOutletStore;

const LED_NAME: &str = "led_cmd";
const LED_HANDLE: u32 = 1;
/// The only payload the stub accepts as decodable (OUT-08 stand-in).
const LED_OK_PAYLOAD: &[u8] = &[7];

impl OutletStore for StubOutletStore {
    fn resolve(&self, name: &str) -> Option<u32> {
        (name == LED_NAME).then_some(LED_HANDLE)
    }

    fn type_id(&self, h: u32) -> Option<u32> {
        (h == LED_HANDLE).then_some(OUTLET_TYPE_ID)
    }

    fn write(&self, h: u32, bytes: &[u8]) -> StoreWrite {
        if h != LED_HANDLE {
            return StoreWrite::InvalidHandle;
        }
        if bytes == LED_OK_PAYLOAD {
            StoreWrite::Ok
        } else {
            StoreWrite::Rejected
        }
    }

    fn list_len(&self) -> u32 {
        1
    }

    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        (index == 0).then(|| (LED_NAME.to_string(), 0))
    }
}

fn spawn_server_with_outlets(listener: UnixListener) {
    let store: Arc<dyn TapStore> = Arc::new(StubStore);
    let outlets: Arc<dyn OutletStore> = Arc::new(StubOutletStore);
    tokio::spawn(async move {
        let _ = serve(listener, store, Some(outlets)).await;
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn correct_version_handshake() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let _stream = connect_and_handshake(&path).await;
    // If we get here without panic, HelloOk was received.
}

#[tokio::test]
async fn wrong_version_receives_hello_rejected_then_eof() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);

    let mut stream = UnixStream::connect(&path).await.unwrap();
    // Any version != PROTOCOL_VERSION must be rejected. Derive it from the
    // constant so the test can never collide with a real future version.
    write_frame(
        &mut stream,
        &Request::Hello {
            protocol_version: PROTOCOL_VERSION.wrapping_add(1),
        },
    )
    .await
    .unwrap();

    let frame = read_frame(&mut stream).await.expect("read HelloRejected");
    let resp: Response = decode_frame(&frame).expect("decode");
    assert_eq!(
        resp,
        Response::HelloRejected {
            supported_version: PROTOCOL_VERSION
        }
    );

    // Server should close the connection after rejection.
    let mut buf = [0u8; 1];
    let n = stream.read(&mut buf).await.expect("read after rejection");
    assert_eq!(n, 0, "expected EOF after HelloRejected");
}

#[tokio::test]
async fn tap_resolve_found() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(
        &mut stream,
        &Request::TapResolve {
            name: TEMP_NAME.into(),
        },
    )
    .await;
    assert_eq!(
        resp,
        Response::Handle {
            handle: TEMP_HANDLE
        }
    );
}

#[tokio::test]
async fn tap_resolve_not_found() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(
        &mut stream,
        &Request::TapResolve {
            name: "missing".into(),
        },
    )
    .await;
    assert_eq!(resp, Response::NotFound);
}

#[tokio::test]
async fn tap_read_retained_value() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(
        &mut stream,
        &Request::TapReadRetained {
            handle: TEMP_HANDLE,
        },
    )
    .await;
    assert_eq!(
        resp,
        Response::Retained {
            timestamp_ms: 42,
            bytes: vec![1, 2, 3]
        }
    );
}

#[tokio::test]
async fn tap_read_retained_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(
        &mut stream,
        &Request::TapReadRetained { handle: HUM_HANDLE },
    )
    .await;
    assert_eq!(resp, Response::Empty);
}

#[tokio::test]
async fn tap_read_retained_invalid_handle() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(&mut stream, &Request::TapReadRetained { handle: 999 }).await;
    assert_eq!(resp, Response::InvalidHandle);
}

#[tokio::test]
async fn tap_take_event_value() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(
        &mut stream,
        &Request::TapTakeEvent {
            handle: TEMP_HANDLE,
        },
    )
    .await;
    // TapTakeEvent → Event{bytes} per spec §4 table.
    assert_eq!(resp, Response::Event { bytes: vec![7, 8] });
}

#[tokio::test]
async fn tap_take_event_invalid_handle() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(&mut stream, &Request::TapTakeEvent { handle: 0 }).await;
    assert_eq!(resp, Response::InvalidHandle);
}

#[tokio::test]
async fn tap_drain_batch_always_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(
        &mut stream,
        &Request::TapDrainBatch {
            handle: TEMP_HANDLE,
        },
    )
    .await;
    assert_eq!(resp, Response::Empty);
}

// ── Spec §4 handle-0 and D1 drain-batch invariants ───────────────────────────
//
// SR-11: "Any handle not present in the host's virtual-handle table — including
// 0 and fabricated values — gets `InvalidHandle`."
// D1: TapDrainBatch always returns `Empty` in v1, regardless of handle value.

/// handle-0 for `TapReadRetained` must return `InvalidHandle` (spec §4: "handle 0
/// is reserved as the invalid/unresolved sentinel").
#[tokio::test]
async fn tap_read_retained_handle_zero_returns_invalid_handle() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(&mut stream, &Request::TapReadRetained { handle: 0 }).await;
    assert_eq!(
        resp,
        Response::InvalidHandle,
        "handle 0 must always return InvalidHandle for TapReadRetained"
    );
}

/// D1: `TapDrainBatch` with handle-0 must return `Empty` (D1 overrides the
/// normal invalid-handle path — drain-batch is always `Empty` in v1).
#[tokio::test]
async fn tap_drain_batch_handle_zero_returns_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(&mut stream, &Request::TapDrainBatch { handle: 0 }).await;
    assert_eq!(
        resp,
        Response::Empty,
        "TapDrainBatch must always return Empty regardless of handle (D1)"
    );
}

/// D1: `TapDrainBatch` with a fabricated invalid non-zero handle must return
/// `Empty` (drain-batch is always `Empty` in v1 — no `InvalidHandle` path).
#[tokio::test]
async fn tap_drain_batch_invalid_handle_returns_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(&mut stream, &Request::TapDrainBatch { handle: 9999 }).await;
    assert_eq!(
        resp,
        Response::Empty,
        "TapDrainBatch with invalid handle must still return Empty (D1)"
    );
}

#[tokio::test]
async fn tap_list_len() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(&mut stream, &Request::TapListLen).await;
    assert_eq!(resp, Response::Count { count: 2 });
}

#[tokio::test]
async fn tap_list_entry_in_range() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(&mut stream, &Request::TapListEntry { index: 0 }).await;
    assert_eq!(
        resp,
        Response::Entry {
            name: TEMP_NAME.into(),
            kind: 0
        }
    );
}

#[tokio::test]
async fn tap_list_entry_out_of_range() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(&mut stream, &Request::TapListEntry { index: 99 }).await;
    assert_eq!(resp, Response::OutOfRange);
}

#[tokio::test]
async fn outlet_resolve_returns_unsupported() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(&mut stream, &Request::OutletResolve { name: "led".into() }).await;
    assert_eq!(resp, Response::Unsupported);
}

#[tokio::test]
async fn outlet_write_returns_unsupported() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;
    let resp = send_request(
        &mut stream,
        &Request::OutletWrite {
            handle: 1,
            bytes: vec![1],
        },
    )
    .await;
    assert_eq!(resp, Response::Unsupported);
}

// ── Outlet dispatch (server with an outlet store) ─────────────────────────────

#[tokio::test]
async fn outlet_resolve_returns_handle_and_not_found() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server_with_outlets(listener);
    let mut stream = connect_and_handshake(&path).await;

    let resp = send_request(
        &mut stream,
        &Request::OutletResolve {
            name: LED_NAME.into(),
        },
    )
    .await;
    assert_eq!(resp, Response::Handle { handle: LED_HANDLE });

    let resp = send_request(
        &mut stream,
        &Request::OutletResolve {
            name: "unknown".into(),
        },
    )
    .await;
    assert_eq!(resp, Response::NotFound);
}

#[tokio::test]
async fn outlet_write_maps_store_results() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server_with_outlets(listener);
    let mut stream = connect_and_handshake(&path).await;

    let resp = send_request(
        &mut stream,
        &Request::OutletWrite {
            handle: LED_HANDLE,
            bytes: LED_OK_PAYLOAD.to_vec(),
        },
    )
    .await;
    assert_eq!(resp, Response::Written);

    let resp = send_request(
        &mut stream,
        &Request::OutletWrite {
            handle: LED_HANDLE,
            bytes: vec![1, 2, 3],
        },
    )
    .await;
    assert_eq!(resp, Response::Rejected);

    let resp = send_request(
        &mut stream,
        &Request::OutletWrite {
            handle: 99,
            bytes: LED_OK_PAYLOAD.to_vec(),
        },
    )
    .await;
    assert_eq!(resp, Response::InvalidHandle);
}

#[tokio::test]
async fn outlet_list_ops_serve_and_bound() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server_with_outlets(listener);
    let mut stream = connect_and_handshake(&path).await;

    let resp = send_request(&mut stream, &Request::OutletListLen).await;
    assert_eq!(resp, Response::Count { count: 1 });

    let resp = send_request(&mut stream, &Request::OutletListEntry { index: 0 }).await;
    assert_eq!(
        resp,
        Response::Entry {
            name: LED_NAME.into(),
            kind: 0
        }
    );

    let resp = send_request(&mut stream, &Request::OutletListEntry { index: 1 }).await;
    assert_eq!(resp, Response::OutOfRange);
}

/// Type-id dispatch (swarm#1315): known handles answer `TypeId`, unknown ones
/// `InvalidHandle`, and outlet type-ids without a store answer `Unsupported`.
#[tokio::test]
async fn type_id_dispatch() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server_with_outlets(listener);
    let mut stream = connect_and_handshake(&path).await;

    let resp = send_request(
        &mut stream,
        &Request::TapTypeId {
            handle: TEMP_HANDLE,
        },
    )
    .await;
    assert_eq!(resp, Response::TypeId { id: TAP_TYPE_ID });

    let resp = send_request(&mut stream, &Request::TapTypeId { handle: 99 }).await;
    assert_eq!(resp, Response::InvalidHandle);

    let resp = send_request(&mut stream, &Request::OutletTypeId { handle: LED_HANDLE }).await;
    assert_eq!(resp, Response::TypeId { id: OUTLET_TYPE_ID });
}

#[tokio::test]
async fn outlet_type_id_unsupported_without_store() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;

    let resp = send_request(&mut stream, &Request::OutletTypeId { handle: 1 }).await;
    assert_eq!(resp, Response::Unsupported);
}

#[tokio::test]
async fn outlet_list_ops_unsupported_without_store() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;

    let resp = send_request(&mut stream, &Request::OutletListLen).await;
    assert_eq!(resp, Response::Unsupported);

    let resp = send_request(&mut stream, &Request::OutletListEntry { index: 0 }).await;
    assert_eq!(resp, Response::Unsupported);
}

/// S1 post-handshake idle timeout: a client that completes Hello then stalls
/// mid-frame (sends nothing) must be dropped after `REQUEST_TIMEOUT_SECS`.
/// Uses `tokio::time::pause` + `advance` for deterministic, zero-wall-clock timing.
#[tokio::test(start_paused = true)]
async fn post_handshake_idle_timeout_drops_connection() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;

    // Client completes Hello successfully, then sends nothing.
    // Advance time past REQUEST_TIMEOUT_SECS (30 s).
    tokio::time::advance(tokio::time::Duration::from_secs(31)).await;

    // Yield to let the server task run and detect the timeout.
    tokio::task::yield_now().await;

    // The server should have closed the connection — we expect EOF.
    let mut buf = [0u8; 1];
    let n = stream
        .read(&mut buf)
        .await
        .expect("read after idle timeout");
    assert_eq!(
        n, 0,
        "server must close connection after post-handshake idle timeout"
    );
}

#[tokio::test]
async fn malformed_frame_closes_connection() {
    let dir = tempfile::TempDir::new().unwrap();
    let (listener, path) = bind_server(&dir);
    spawn_server(listener);
    let mut stream = connect_and_handshake(&path).await;

    // Send a frame with a length that fits in the cap but has garbage payload.
    let garbage: Vec<u8> = {
        let payload = vec![0xFF_u8; 8];
        let len = u32::try_from(payload.len()).expect("payload fits in u32");
        let mut f = len.to_le_bytes().to_vec();
        f.extend_from_slice(&payload);
        f
    };
    stream.write_all(&garbage).await.unwrap();

    // Server should close the connection.
    let mut buf = [0u8; 1];
    // Give the server a moment to process and close.
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let n = stream
        .read(&mut buf)
        .await
        .expect("read after malformed frame");
    assert_eq!(n, 0, "expected EOF after malformed frame");
}
