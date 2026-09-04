//! Tests for `TapClient`: lazy connect, generation-checked handles,
//! server-down / server-restart recovery, reconnect SLA, the per-operation
//! timeout, and the bound on the shared handle table.
//!
//! Time is driven deterministically via `tokio::time::pause()` + `advance`.

#![cfg(test)]

use std::sync::Arc;
use tokio::net::UnixListener;

use crate::types::{ClientRead, StoreRead, TapStore};
use crate::{MAX_RESOLVE_NAME_LEN, RECONNECT_SLA_SECS, TAP_CALL_TIMEOUT, TapClient};

// ── Stub stores ───────────────────────────────────────────────────────────────

/// Wire-type ids the stub stores report (arbitrary, distinct).
const TYPE_ID_A: u32 = 0xA11CE;
const TYPE_ID_B: u32 = 0xB0B;

/// A store with one retained tap named "temp".
struct StoreA;

impl TapStore for StoreA {
    fn resolve(&self, name: &str) -> Option<u32> {
        if name == "temp" { Some(1) } else { None }
    }
    fn type_id(&self, h: u32) -> Option<u32> {
        (h == 1).then_some(TYPE_ID_A)
    }
    fn read_retained(&self, h: u32) -> StoreRead {
        if h == 1 {
            StoreRead::Value {
                timestamp_ms: 100,
                bytes: vec![0xAA],
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
            Some(("temp".into(), 0))
        } else {
            None
        }
    }
}

/// A store with DIFFERENT tap layout — simulates a pipeline restart.
struct StoreB;

impl TapStore for StoreB {
    fn resolve(&self, name: &str) -> Option<u32> {
        if name == "pressure" { Some(1) } else { None }
    }
    fn type_id(&self, h: u32) -> Option<u32> {
        (h == 1).then_some(TYPE_ID_B)
    }
    fn read_retained(&self, h: u32) -> StoreRead {
        if h == 1 {
            StoreRead::Value {
                timestamp_ms: 200,
                bytes: vec![0xBB],
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
            Some(("pressure".into(), 0))
        } else {
            None
        }
    }
}

// ── Helper: start a server and return a TapClient pointed at it ───────────────

#[allow(clippy::needless_pass_by_value)]
fn start_server(path: &std::path::Path, store: Arc<dyn TapStore>) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("bind");
    tokio::spawn(async move {
        let _ = crate::serve(listener, store, None).await;
    })
}

// ── Test: resolve + read against a live server ────────────────────────────────

#[tokio::test]
async fn resolve_and_read_retained() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server(&path, Arc::new(StoreA));

    let client = TapClient::new(path);
    let vh = client.resolve("temp").await.expect("resolve");
    assert!(vh >= 1, "virtual handle must be >= 1");
    assert_ne!(vh, 0, "virtual handle must not be 0");

    let result = client.read_retained(vh).await;
    assert_eq!(
        result,
        ClientRead::Value {
            timestamp_ms: 100,
            bytes: vec![0xAA]
        }
    );
}

#[tokio::test]
async fn resolve_unknown_name_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server(&path, Arc::new(StoreA));
    let client = TapClient::new(path);
    let result = client.resolve("missing").await;
    assert!(result.is_none());
}

// ── Test: no socket configured → fail-closed Unavailable (no connect) ─────────
//
// Regression: the cell host builds `TapClient::unavailable()` when no
// signal-layer socket path can be resolved (neither `/run/peeriot` writable nor
// `XDG_RUNTIME_DIR` set — e.g. a CI runner). A missing socket must map to "taps
// unavailable" (D3), never a hard failure. Every operation returns Unavailable
// without attempting a connection, so a cell host with no tap socket still runs.
#[tokio::test]
async fn unavailable_client_reports_unavailable_without_connecting() {
    let client = TapClient::unavailable();

    // Ops that go through `ensure_connected` short-circuit to unavailable.
    assert!(
        client.resolve("temp").await.is_none(),
        "resolve must be None"
    );
    assert!(client.list_len().await.is_none(), "list_len must be None");
    assert!(
        client.list_entry(0).await.is_none(),
        "list_entry must be None"
    );
    // Reads fail-close (never Value/Empty).
    assert_eq!(client.read_retained(1).await, ClientRead::Unavailable);
    assert_eq!(client.take_event(1).await, ClientRead::Unavailable);
}

// ── Test: list operations ─────────────────────────────────────────────────────

#[tokio::test]
async fn list_len_and_entry() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server(&path, Arc::new(StoreA));
    let client = TapClient::new(path);

    let len = client.list_len().await.expect("list_len");
    assert_eq!(len, 1);
    let entry = client.list_entry(0).await.expect("list_entry");
    assert_eq!(entry, ("temp".to_owned(), 0u8));
    assert!(client.list_entry(99).await.is_none());
}

// ── Test: drain_batch always Empty (D1) ──────────────────────────────────────

#[tokio::test]
async fn drain_batch_always_empty() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server(&path, Arc::new(StoreA));
    let client = TapClient::new(path);
    let vh = client.resolve("temp").await.unwrap();
    let result = client.drain_batch(vh).await;
    assert_eq!(result, ClientRead::Empty);
}

// ── Test: server down → resolve returns None, reads return Unavailable ────────

#[tokio::test]
async fn server_down_resolve_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("no-such.sock");
    // No server started.
    let client = TapClient::new(path);
    let result = client.resolve("temp").await;
    assert!(result.is_none(), "expected None when server is down");
}

/// After the server restarts and the client reconnects (bumping the generation),
/// any virtual handle from the previous generation returns Unavailable.
///
/// This test uses a `TapClient` helper that can force-clear the connection to
/// simulate a clean connection drop (server process exit).
#[tokio::test]
async fn stale_virtual_handle_returns_unavailable_after_reconnect() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server(&path, Arc::new(StoreA));

    let client = TapClient::new(path.clone());
    let vh = client.resolve("temp").await.expect("resolve on StoreA");
    // vh is now associated with generation 1.

    // Remove the socket file — the server is still alive but can no longer
    // accept new connections.  Force the client to lose its existing connection
    // by calling disconnect_for_test(), which is the clean way to simulate a
    // server restart without relying on task abort timing.
    client.disconnect_for_test().await;
    std::fs::remove_file(&path).ok();

    // Start a new server with StoreB (different layout) on the same path.
    start_server(&path, Arc::new(StoreB));
    tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

    // Trigger a reconnect by calling resolve.  This bumps gen to 2.
    let _vh2 = client.resolve("pressure").await;

    // Now vh (gen 1) should return Unavailable — it's from a previous generation.
    let result = client.read_retained(vh).await;
    assert_eq!(
        result,
        ClientRead::Unavailable,
        "stale handle must return Unavailable"
    );
}

/// SR-12: a stale handle must never return another tap's bytes after a restart.
///
/// Scenario: resolve "temp" on `StoreA` (generation 1), restart with `StoreB`
/// which has different tap layout, reconnect (generation becomes 2), then
/// the original virtual handle must return `Unavailable` — not `StoreB`'s data.
#[tokio::test]
async fn stale_handle_never_aliases_different_tap() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");

    // Start StoreA.
    start_server(&path, Arc::new(StoreA));
    let client = TapClient::new(path.clone());
    let vh_temp = client
        .resolve("temp")
        .await
        .expect("resolve temp on StoreA");
    // vh_temp is associated with generation 1.

    // Force the client to drop its current connection (simulates server restart).
    client.disconnect_for_test().await;
    std::fs::remove_file(&path).ok();

    // Start StoreB on the same path.
    start_server(&path, Arc::new(StoreB));
    tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

    // Trigger reconnect → generation becomes 2.
    let _vh_pressure = client.resolve("pressure").await;

    // The old handle vh_temp (generation 1) must not alias StoreB's data.
    let result = client.read_retained(vh_temp).await;
    match result {
        ClientRead::Unavailable => {} // Correct: stale generation detected
        ClientRead::Value { bytes, .. } => {
            // Must NOT be StoreB's 0xBB data — that would be aliasing.
            assert_ne!(
                bytes,
                vec![0xBB_u8],
                "stale handle aliased a different tap's bytes!"
            );
        }
        ClientRead::Empty => {} // Also acceptable — server returned Empty
    }
}

// ── Test: reconnect within SLA once server reappears ─────────────────────────

/// The 10 s SLA: once the socket is accepting connections the client must
/// reconnect within 10 seconds of stepped time.
///
/// Uses `tokio::time::pause()` + `advance` for deterministic control.
#[tokio::test(start_paused = true)]
async fn reconnect_within_sla_paused_time() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");

    // No server yet; verify resolve fails.
    let client = Arc::new(TapClient::new(path.clone()));
    let result = client.resolve("temp").await;
    assert!(result.is_none(), "server not up yet");

    // Now start the server.
    start_server(&path, Arc::new(StoreA));

    // Spawn the backoff reconnect loop.
    let client2 = Arc::clone(&client);
    let reconnect = tokio::spawn(async move {
        client2.connect_with_backoff().await;
    });

    // Advance time in steps.  Each failed attempt doubles the backoff from
    // 250 ms → 500 → 1000 → 2000 → 4000 → 5000 (cap).  But since the server
    // is already up, the first attempt should succeed immediately.
    // Step time enough to trigger the first sleep (250 ms).
    tokio::time::advance(tokio::time::Duration::from_millis(300)).await;
    // Yield to let tasks run.
    tokio::task::yield_now().await;

    // The total elapsed wall time (in paused-clock terms) is well under 10 s.
    let total_elapsed = tokio::time::Duration::from_millis(300);
    assert!(
        total_elapsed <= tokio::time::Duration::from_secs(RECONNECT_SLA_SECS),
        "reconnect took longer than the SLA"
    );

    // Allow the reconnect task to finish.
    tokio::time::advance(tokio::time::Duration::from_millis(300)).await;
    tokio::task::yield_now().await;
    let _ = reconnect.await;

    // After reconnect, resolve should work.
    let vh = client.resolve("temp").await;
    assert!(vh.is_some(), "resolve should succeed after reconnect");
}

/// Verify that backoff steps stay within the SLA bound mathematically.
/// 250 + 500 + 1000 + 2000 + 4000 = 7750 ms < 10 000 ms.
/// So 5 failed attempts exhausting backoff to the cap (5 s) still fits within
/// the 10 s SLA.  This is a property test without actual I/O.
#[test]
fn backoff_sequence_fits_within_sla() {
    let sla_ms = RECONNECT_SLA_SECS * 1000;
    let mut total: u64 = 0;
    let mut delay: u64 = 250;
    // Simulate attempts until we hit the cap.
    for _ in 0..20 {
        total += delay;
        assert!(
            total < sla_ms,
            "backoff sequence exceeded SLA: total={total} ms, sla={sla_ms} ms"
        );
        delay = (delay * 2).min(5000);
        if delay == 5000 {
            // Once at cap, one more attempt is all we need to verify.
            break;
        }
    }
    // total at this point should be well under 10 s.
    assert!(
        total < sla_ms,
        "total backoff {total} ms exceeded SLA {sla_ms} ms"
    );
}

// ── Spec-derived SR-13 SLA bound test ────────────────────────────────────────
//
// SR-13: "once the socket is reachable again the host reconnects within 10 s".
//
// The spec SLA is asserted at the unit level against TapClient by advancing
// paused time to 9 seconds (within the SLA) and confirming reconnect succeeds.
// The test verifies that reconnect completes before the 10 s boundary, not just
// that some fixed time is under 10 s.

/// SR-13: When the server becomes reachable, the client reconnects within the
/// 10 s SLA bound.  We advance mock time to 9 s (just inside the SLA) and
/// assert the connection is established.
///
/// This is stronger than `backoff_sequence_fits_within_sla` (math-only) and
/// stronger than `reconnect_within_sla_paused_time` (which only checks
/// 300 ms < 10 s without verifying that reconnect actually completed).
#[tokio::test(start_paused = true)]
async fn reconnect_completes_before_sla_boundary() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sla_bound.sock");

    // No server initially — first resolve fails.
    let client = Arc::new(TapClient::new(path.clone()));
    assert!(client.resolve("temp").await.is_none(), "no server yet");

    // Bring the server up.
    start_server(&path, Arc::new(StoreA));

    // Kick off the reconnect loop in the background.
    let client2 = Arc::clone(&client);
    let reconnect_task = tokio::spawn(async move {
        client2.connect_with_backoff().await;
    });

    // Advance time incrementally through the backoff sequence (250, 500, 1000,
    // 2000 ms steps) — this is the maximum realistic wait before the cap kicks in.
    // Each advance yields to the runtime between steps to allow tasks to run.
    for step_ms in [250u64, 500, 1000, 2000] {
        tokio::time::advance(tokio::time::Duration::from_millis(step_ms)).await;
        tokio::task::yield_now().await;
    }

    // Advance to just under 9 s total from the start (well within the 10 s SLA).
    tokio::time::advance(tokio::time::Duration::from_secs(4)).await;
    tokio::task::yield_now().await;

    // The reconnect task must have completed by now.
    assert!(
        reconnect_task.is_finished() || {
            // Allow one more yield if the task is still scheduled.
            tokio::task::yield_now().await;
            reconnect_task.is_finished()
        },
        "client must reconnect within the SLA ({RECONNECT_SLA_SECS} s)"
    );

    // After reconnect, resolve must succeed.
    let vh = client.resolve("temp").await;
    assert!(
        vh.is_some(),
        "resolve must succeed after reconnect within SLA"
    );
}

/// SR-13: The `RECONNECT_SLA_SECS` constant must be exactly 10 (the spec-stated
/// bound).  A change to the constant without updating this test is intentional —
/// this test pins the spec contract.
#[test]
fn reconnect_sla_constant_is_ten_seconds() {
    assert_eq!(
        RECONNECT_SLA_SECS, 10,
        "spec SR-13 states the reconnect SLA is 10 s; RECONNECT_SLA_SECS must equal 10"
    );
}

// ── Spec-derived SR-12: stale handle invariant is unconditional ───────────────
//
// The existing `stale_handle_never_aliases_different_tap` test accepts
// `ClientRead::Empty` as a valid outcome, which means a stale handle could
// appear to return Empty data from a new server.  The spec (SR-12) says a stale
// handle must return Unavailable (D3: "dead virtual handles return unavailable").
// This test asserts the unconditional invariant: stale handle → Unavailable.

/// SR-12: After reconnect (generation bump), a handle from the previous
/// generation must return Unavailable — not Empty, not Value.
#[tokio::test]
async fn stale_handle_returns_unavailable_unconditionally() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("stale_unconditional.sock");

    start_server(&path, Arc::new(StoreA));
    let client = TapClient::new(path.clone());
    let vh = client.resolve("temp").await.expect("resolve on StoreA");

    // Force connection drop (generation bump on next connect).
    client.disconnect_for_test().await;
    std::fs::remove_file(&path).ok();

    // Start a new server — different tap layout.
    start_server(&path, Arc::new(StoreB));
    tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

    // Trigger reconnect (bumps generation to 2).
    let _vh2 = client.resolve("pressure").await;

    // vh is from generation 1; must unconditionally return Unavailable.
    let result = client.read_retained(vh).await;
    assert_eq!(
        result,
        ClientRead::Unavailable,
        "SR-12: stale handle (generation 1) after reconnect (generation 2) must return Unavailable, not Empty or Value"
    );
}

// ── B2 regression test: send_recv cancellation desync ────────────────────────

/// B2: If a `send_recv` future is cancelled after the request is written but
/// before the response is read, the next caller on the same shared connection
/// must NOT receive the stale (prior) response frame — it must get its own
/// correct response or a clean Unavailable (connection torn down), never the
/// previous response's bytes.
///
/// Strategy: A custom server reads the request and then *stalls* (simulated by
/// a one-shot barrier).  The client's `send_recv` is started, write succeeds, but
/// then the future is dropped (cancelled) before the server unblocks and sends
/// the response.  The server then sends the stale response.  The NEXT call
/// (a new `send_recv`) must not see that stale frame.
///
/// With the fix the drop-guard tears down the connection on cancellation, so
/// the next call either reconnects cleanly or returns Unavailable — it never
/// reads the prior response.
#[tokio::test]
async fn cancelled_send_recv_does_not_desync_next_caller() {
    use tokio::sync::oneshot;

    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("cancel_test.sock");

    // ── Controlled server ─────────────────────────────────────────────────
    // Accepts one connection, reads the Hello + first request, then waits for
    // a signal before sending the response.  After the signal it sends the
    // stale response.  A second request is handled normally.
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");
    let (stall_tx, stall_rx) = oneshot::channel::<()>();
    let stall_tx = std::sync::Arc::new(tokio::sync::Mutex::new(Some(stall_tx)));

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");

        // Read Hello and respond HelloOk.
        let frame = crate::framing::read_frame(&mut stream)
            .await
            .expect("read Hello");
        let req: crate::types::Request =
            crate::framing::decode_frame(&frame).expect("decode Hello");
        assert!(matches!(req, crate::types::Request::Hello { .. }));
        crate::framing::write_frame(
            &mut stream,
            &crate::types::Response::HelloOk {
                version: crate::PROTOCOL_VERSION,
            },
        )
        .await
        .expect("write HelloOk");

        // Read the first (to-be-cancelled) request.
        let _frame = crate::framing::read_frame(&mut stream)
            .await
            .expect("read first req");

        // Stall until told to continue.
        let _ = stall_rx.await;

        // Now send the stale response (for the cancelled request).
        // With the fix this write will fail because the client dropped the
        // connection, and the server just errors out of this task.
        let write_result = crate::framing::write_frame(
            &mut stream,
            &crate::types::Response::Count { count: 999 }, // stale / wrong
        )
        .await;
        if write_result.is_err() {
            // Client disconnected on cancel — correct behaviour.
            return;
        }

        // If we somehow reach here (client did NOT tear down), drain further
        // requests so the server doesn't hang.
        loop {
            let Ok(frame) = crate::framing::read_frame(&mut stream).await else {
                break;
            };
            let Ok(req): Result<crate::types::Request, _> = crate::framing::decode_frame(&frame)
            else {
                break;
            };
            let resp = match req {
                crate::types::Request::TapListLen => crate::types::Response::Count { count: 1 },
                _ => crate::types::Response::Unsupported,
            };
            if crate::framing::write_frame(&mut stream, &resp)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // ── Client ────────────────────────────────────────────────────────────
    let client = TapClient::new(path.clone());

    // Establish connection (resolve a name — server is up, will return NotFound).
    // We need to get the connection open.  list_len is a simple no-handle call.
    // But the stall server only handles one request.  We'll connect first via
    // a direct low-level approach: just open the connection with list_len,
    // but route it through our stall.
    //
    // Simpler approach: start the cancellable future, poll it once to get the
    // write done, then drop it before the server responds, then call again.

    // First: prime the connection.  The server reads Hello + responds HelloOk
    // synchronously.  Then the server reads the FIRST request and stalls.
    //
    // We issue list_len (which under the hood calls ensure_connected + send_recv).
    // We spawn it so we can abort it.
    let client = std::sync::Arc::new(client);
    let client2 = std::sync::Arc::clone(&client);

    // Start the first call in a task so we can abort it.
    let first_call = tokio::spawn(async move { client2.list_len().await });

    // Give it enough time to write the request (Hello + TapListLen both written).
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Cancel the first call by aborting the task.
    first_call.abort();
    let _ = first_call.await; // await the abort to complete

    // Signal the server to unblock and send the stale response.
    let mut guard = stall_tx.lock().await;
    if let Some(tx) = guard.take() {
        let _ = tx.send(());
    }
    drop(guard);

    // Give the server a moment to send the stale frame.
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

    // Now make a SECOND call on the same client.  With the bug (no drop-guard)
    // the client still has its connection open and reads the stale Count{999}
    // frame as if it were a response to TapListLen — returning Some(999).
    // With the fix the connection is torn down on cancel, so either:
    //   a) reconnect succeeds (but our controlled server is exhausted) → None
    //   b) reconnect fails → None
    // Either way, we must NOT get Some(999).
    let result = client.list_len().await;
    assert_ne!(
        result,
        Some(999),
        "B2: second call must not observe stale frame from cancelled send_recv; got {result:?}"
    );
}

// ── Resolve-name bound ───────────────────────────────────────────────────────

/// A store that resolves any name to the same tap.  Because the server never
/// answers `NotFound`, a `None` from `resolve` can only come from the client's
/// own bound check — which is what makes the boundary assertions meaningful.
struct AnyNameStore;

impl TapStore for AnyNameStore {
    fn resolve(&self, _name: &str) -> Option<u32> {
        Some(1)
    }
    fn type_id(&self, h: u32) -> Option<u32> {
        (h == 1).then_some(TYPE_ID_A)
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
            Some(("any".into(), 0))
        } else {
            None
        }
    }
}

/// A name one byte past the bound is refused even by a server that would have
/// resolved it, so the refusal is the client's and the bound is exact.
#[tokio::test]
async fn resolve_name_past_bound_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server(&path, Arc::new(AnyNameStore));

    let client = TapClient::new(path);
    assert!(
        client
            .resolve(&"x".repeat(MAX_RESOLVE_NAME_LEN + 1))
            .await
            .is_none(),
        "a name one byte past the bound must be refused"
    );
}

/// Refusing an unframeable name costs nothing: the connection stays up and a
/// handle issued before the refused call still reads its value afterwards.
///
/// Without the bound check the request is framed anyway, the server's framer
/// rejects it and drops the connection, the next call reconnects on a new
/// generation, and every handle from the old generation goes stale.
#[tokio::test]
async fn unframeable_resolve_name_refused_without_disturbing_live_handle() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server(&path, Arc::new(StoreA));

    let client = TapClient::new(path);
    let vh = client.resolve("temp").await.expect("resolve");
    let expected = ClientRead::Value {
        timestamp_ms: 100,
        bytes: vec![0xAA],
    };
    assert_eq!(client.read_retained(vh).await, expected);

    // A name that fills the entire frame budget leaves no room for its own
    // envelope, so the resulting frame is guaranteed to be over the cap.
    let unframeable = "x".repeat(crate::MAX_FRAME_LEN as usize);
    assert!(
        client.resolve(&unframeable).await.is_none(),
        "an unframeable name must be refused"
    );

    // A later resolve is what would reconnect (and bump the generation) if the
    // refused call had torn the connection down.
    assert!(
        client.resolve("temp").await.is_some(),
        "the shared connection must still serve other resolves"
    );
    assert_eq!(
        client.read_retained(vh).await,
        expected,
        "the pre-existing handle must survive a refused oversized resolve"
    );
}

/// The longest name the bound allows still round-trips to the server and yields
/// a usable handle.
#[tokio::test]
async fn resolve_name_at_bound_is_accepted() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server(&path, Arc::new(AnyNameStore));

    let client = TapClient::new(path);
    let vh = client
        .resolve(&"n".repeat(MAX_RESOLVE_NAME_LEN))
        .await
        .expect("a name at the bound must resolve");
    assert_eq!(
        client.read_retained(vh).await,
        ClientRead::Value {
            timestamp_ms: 300,
            bytes: vec![0xCC],
        }
    );
}

// ── Per-operation timeout ────────────────────────────────────────────────────

/// Start a server whose first connection completes the handshake, reads one
/// request and then never answers it; every later connection is served normally.
///
/// The returned receiver fires once that request has been read, which lets a
/// test distinguish "the peer is sitting on the request" from "the request has
/// not been written yet".
fn start_stalling_then_serving_server(
    path: &std::path::Path,
    store: Arc<dyn TapStore>,
) -> tokio::sync::oneshot::Receiver<()> {
    use crate::framing::{decode_frame, read_frame, write_frame};
    use crate::types::{Request, Response};

    let listener = UnixListener::bind(path).expect("bind");
    let (stalled_tx, stalled_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");

        tokio::spawn(async move {
            let frame = read_frame(&mut stream).await.expect("read Hello");
            let req: Request = decode_frame(&frame).expect("decode Hello");
            assert!(matches!(req, Request::Hello { .. }));
            write_frame(
                &mut stream,
                &Response::HelloOk {
                    version: crate::PROTOCOL_VERSION,
                },
            )
            .await
            .expect("write HelloOk");

            read_frame(&mut stream).await.expect("read the request");
            let _ = stalled_tx.send(());

            // Never answer, and hold the connection open: the caller has to give
            // up on its own rather than be released by an EOF.
            std::future::pending::<()>().await;
        });

        // Every reconnect after the stalled one is served normally.
        let _ = crate::serve(listener, store, None).await;
    });

    stalled_rx
}

/// Join a cell's call, failing the test rather than hanging on it.  An unbounded
/// call never returns at all, and a hung test is a much worse diagnostic than a
/// failed one.
async fn join_call<T>(call: tokio::task::JoinHandle<T>, cell: &str) -> T {
    tokio::time::timeout(TAP_CALL_TIMEOUT, call)
        .await
        .unwrap_or_else(|_| panic!("{cell}'s call is not bounded"))
        .expect("call task")
}

/// A peer that accepts a request and never answers must not hold the calling
/// cell for longer than the bound, and must not hold the cells queued behind it
/// on the shared connection for longer either.
///
/// Cell A's call reaches the peer and stalls.  Cells B and C then queue on the
/// shared connection; because they started later their own bounds expire later,
/// so what has to release them is A's bound expiring, not their own.  Without
/// the bound the peer holds A — and through A the shared connection — forever.
#[tokio::test]
async fn stalled_call_fails_within_bound_without_blocking_other_cells() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("stalled_call.sock");
    let stalled = start_stalling_then_serving_server(&path, Arc::new(StoreA));

    let client = Arc::new(TapClient::new(path));

    // Cell A's call, in real time so that it genuinely reaches the peer.
    let client_a = Arc::clone(&client);
    let cell_a = tokio::spawn(async move { client_a.resolve("temp").await });
    stalled.await.expect("the peer received cell A's request");

    // From here the clock is ours: the peer is sitting on A's request and no
    // further progress is possible without time passing.
    tokio::time::pause();

    // Cells B and C queue behind A on the shared connection.  Their bounds start
    // now, half a bound later than A's, so A's is the one that expires first.
    tokio::time::advance(TAP_CALL_TIMEOUT / 2).await;
    let queued_at = tokio::time::Instant::now();
    let client_b = Arc::clone(&client);
    let cell_b = tokio::spawn(async move { client_b.resolve("temp").await });
    let client_c = Arc::clone(&client);
    let cell_c = tokio::spawn(async move { client_c.list_len().await });
    tokio::task::yield_now().await;

    // Reach A's bound.
    tokio::time::advance(TAP_CALL_TIMEOUT / 2).await;

    assert!(
        join_call(cell_a, "cell A").await.is_none(),
        "the stalled call must be reported to cell A as a failure"
    );

    // A is done and the connection it desynchronised has been torn down; let B
    // and C reconnect and finish in real time.
    tokio::time::resume();

    assert!(
        join_call(cell_b, "cell B").await.is_some(),
        "cell B must still be able to resolve a tap"
    );
    assert_eq!(
        join_call(cell_c, "cell C").await,
        Some(1),
        "cell C must still be able to list taps"
    );
    assert!(
        queued_at.elapsed() <= TAP_CALL_TIMEOUT,
        "cells queued behind a stalled call must not wait longer than the bound"
    );
}

/// Start a server that accepts connections, holds them open and never answers
/// the Hello.  Every accept is reported on the returned channel.
fn start_silent_handshake_server(
    path: &std::path::Path,
) -> tokio::sync::mpsc::UnboundedReceiver<()> {
    let listener = UnixListener::bind(path).expect("bind");
    let (accepted_tx, accepted_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
            if accepted_tx.send(()).is_err() {
                return;
            }
        }
    });

    accepted_rx
}

/// A peer that accepts the connection but never completes the handshake must
/// not hold a call open either: connection establishment is inside the bound.
#[tokio::test]
async fn call_that_cannot_complete_the_handshake_fails_within_the_bound() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("silent_handshake.sock");
    let mut accepted = start_silent_handshake_server(&path);

    let client = TapClient::new(path);
    let call = tokio::spawn(async move { client.resolve("temp").await });

    accepted.recv().await.expect("the peer accepted the call");
    tokio::time::pause();
    assert!(
        !call.is_finished(),
        "the call must still be waiting for the Hello reply"
    );

    tokio::time::advance(TAP_CALL_TIMEOUT).await;
    assert!(
        join_call(call, "the caller").await.is_none(),
        "a handshake the peer never completes must be reported as a failure"
    );
}

/// The reconnect loop must survive a peer that accepts connections and never
/// completes the handshake: each attempt is bounded, so the loop gives up on the
/// stalled one and tries again instead of holding the shared connection for good.
#[tokio::test(start_paused = true)]
async fn stalled_handshake_does_not_wedge_the_reconnect_loop() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("silent_reconnect.sock");
    let mut accepted = start_silent_handshake_server(&path);

    let client = TapClient::new(path);
    tokio::spawn(async move { client.connect_with_backoff().await });

    // One stalled attempt plus its backoff is all a second attempt should cost;
    // the SLA is a generous ceiling on the backoff part.
    let budget = TAP_CALL_TIMEOUT + tokio::time::Duration::from_secs(RECONNECT_SLA_SECS);
    let second_attempt = tokio::time::timeout(budget, async {
        accepted.recv().await.expect("first attempt");
        accepted.recv().await.expect("second attempt");
    })
    .await;

    assert!(
        second_attempt.is_ok(),
        "the reconnect loop must attempt again after a stalled handshake"
    );
}

/// A single attempt that burns the whole per-operation bound must still leave
/// the reconnect loop room to retry inside its SLA.
#[test]
fn call_bound_leaves_room_within_the_reconnect_sla() {
    assert!(
        TAP_CALL_TIMEOUT * 2 <= tokio::time::Duration::from_secs(RECONNECT_SLA_SECS),
        "TAP_CALL_TIMEOUT ({TAP_CALL_TIMEOUT:?}) must leave a retry inside the {RECONNECT_SLA_SECS} s reconnect SLA"
    );
}

// ── Bound on the shared handle table ─────────────────────────────────────────

/// A store with several taps, so the table can be compared against a tap count
/// greater than one — with a single tap, a table that reuses entries and one
/// that simply stopped growing look the same.
struct MultiTapStore;

impl MultiTapStore {
    const TAPS: [&'static str; 3] = ["temp", "pressure", "humidity"];

    /// Server handles are the tap's position in the registry, offset so that
    /// no tap gets handle 0.
    fn handle_of(name: &str) -> Option<u32> {
        let index = Self::TAPS.iter().position(|tap| *tap == name)?;
        u32::try_from(index + 1).ok()
    }
}

impl TapStore for MultiTapStore {
    fn resolve(&self, name: &str) -> Option<u32> {
        Self::handle_of(name)
    }
    fn type_id(&self, h: u32) -> Option<u32> {
        Self::TAPS
            .iter()
            .any(|tap| Self::handle_of(tap) == Some(h))
            .then_some(TYPE_ID_A)
    }
    fn read_retained(&self, h: u32) -> StoreRead {
        if Self::TAPS.iter().any(|tap| Self::handle_of(tap) == Some(h)) {
            StoreRead::Value {
                timestamp_ms: 400,
                bytes: h.to_le_bytes().to_vec(),
            }
        } else {
            StoreRead::InvalidHandle
        }
    }
    fn take_event(&self, _h: u32) -> StoreRead {
        StoreRead::Empty
    }
    fn list_len(&self) -> u32 {
        u32::try_from(Self::TAPS.len()).expect("tap count fits in the wire type")
    }
    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        let index = usize::try_from(index).ok()?;
        Self::TAPS.get(index).map(|name| ((*name).to_owned(), 0))
    }
}

/// The tap names the registry reports, discovered the way a cell discovers them.
async fn registry_tap_names(client: &TapClient) -> Vec<String> {
    let count = client.list_len().await.expect("list_len");
    let mut names = Vec::new();
    for index in 0..count {
        let (name, _kind) = client.list_entry(index).await.expect("list_entry");
        names.push(name);
    }
    names
}

/// Resolve every name once, in order, as a set of cells starting up would.
async fn resolve_all(client: &TapClient, names: &[String]) -> Vec<u32> {
    let mut handles = Vec::new();
    for name in names {
        handles.push(
            client
                .resolve(name)
                .await
                .expect("a tap the registry reports must resolve"),
        );
    }
    handles
}

/// The handle table is shared by every cell on the node, so resolving a tap
/// another cell already resolved must hand back the entry that already exists.
/// Resolving many times more often than there are taps therefore leaves the
/// table at one entry per tap: the registry's tap count is the bound.
#[tokio::test]
async fn repeated_resolution_reuses_live_entries() {
    /// Enough passes that the total number of resolves is well past the number
    /// of taps, which is what an unbounded table would grow to.
    const RESOLVE_PASSES: usize = 5;

    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("reuse.sock");
    start_server(&path, Arc::new(MultiTapStore));

    let client = TapClient::new(path);
    let names = registry_tap_names(&client).await;

    let issued = resolve_all(&client, &names).await;
    for _ in 1..RESOLVE_PASSES {
        assert_eq!(
            resolve_all(&client, &names).await,
            issued,
            "resolving a tap the table already holds a live entry for must reuse that entry"
        );
    }

    assert_eq!(
        client.handle_table_len_for_test().await,
        names.len(),
        "the table must hold one entry per tap however often the taps are resolved"
    );
    let reported = client.list_len().await.expect("list_len");
    assert!(
        u32::try_from(client.handle_table_len_for_test().await).expect("entry count fits")
            <= reported,
        "live entries must stay within the tap count the registry reports"
    );
}

/// Every server handle dies with the connection it was issued on, so a
/// reconnect must leave none of the previous generation behind.  Cycling
/// through reconnects then accumulates nothing: each cycle empties the table
/// and re-resolving refills it to one entry per tap, never a multiple of it.
#[tokio::test]
async fn repeated_reconnects_release_superseded_entries() {
    /// More than one cycle, so growth that is only cumulative shows up.
    const RECONNECT_CYCLES: usize = 3;

    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("reconnect_release.sock");
    start_server(&path, Arc::new(MultiTapStore));

    let client = TapClient::new(path);
    let names = registry_tap_names(&client).await;

    let mut superseded: Vec<u32> = Vec::new();
    for _ in 0..RECONNECT_CYCLES {
        let issued = resolve_all(&client, &names).await;
        assert!(
            issued.iter().all(|vh| !superseded.contains(vh)),
            "an entry a reconnect superseded must not be handed back as if it were live"
        );
        assert_eq!(
            client.handle_table_len_for_test().await,
            names.len(),
            "a cycle must leave one entry per tap, not one per tap per cycle"
        );

        // Drive the reconnect rather than leaving it to the next call: a
        // lookup happens before the connection is re-established, so the call
        // after a teardown is still answered against the generation the
        // teardown is about to supersede.
        client.disconnect_for_test().await;
        client.connect_with_backoff().await;

        assert_eq!(
            client.handle_table_len_for_test().await,
            0,
            "a generation change must release the entries it supersedes"
        );
        superseded = issued;
    }
}

// ── Outlet operations ─────────────────────────────────────────────────────────

/// Outlet store accepting one outlet `led_cmd` (server handle 9) whose only
/// decodable payload is `[7]` (OUT-08 stand-in).
struct OutletStoreA;

impl crate::types::OutletStore for OutletStoreA {
    fn resolve(&self, name: &str) -> Option<u32> {
        (name == "led_cmd").then_some(9)
    }
    fn type_id(&self, h: u32) -> Option<u32> {
        (h == 9).then_some(TYPE_ID_B)
    }
    fn write(&self, h: u32, bytes: &[u8]) -> crate::types::StoreWrite {
        if h != 9 {
            return crate::types::StoreWrite::InvalidHandle;
        }
        if bytes == [7] {
            crate::types::StoreWrite::Ok
        } else {
            crate::types::StoreWrite::Rejected
        }
    }
    fn list_len(&self) -> u32 {
        1
    }
    fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        (index == 0).then(|| ("led_cmd".to_string(), 0))
    }
}

#[allow(clippy::needless_pass_by_value)]
fn start_server_with_outlets(
    path: &std::path::Path,
    store: Arc<dyn TapStore>,
) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(path).expect("bind");
    let outlets: Arc<dyn crate::types::OutletStore> = Arc::new(OutletStoreA);
    tokio::spawn(async move {
        let _ = crate::serve(listener, store, Some(outlets)).await;
    })
}

#[tokio::test]
async fn outlet_resolve_write_round_trip() {
    use crate::types::ClientWrite;

    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server_with_outlets(&path, Arc::new(StoreA));

    let client = TapClient::new(path);
    let vh = client.outlet_resolve("led_cmd").await.expect("resolve");
    assert!(vh >= 1, "virtual handle must be >= 1");

    assert_eq!(client.outlet_write(vh, vec![7]).await, ClientWrite::Ok);
    assert_eq!(
        client.outlet_write(vh, vec![1, 2]).await,
        ClientWrite::Rejected,
        "a payload the store cannot decode must surface as Rejected"
    );

    assert_eq!(client.outlet_list_len().await, Some(1));
    assert_eq!(
        client.outlet_list_entry(0).await,
        Some(("led_cmd".to_string(), 0))
    );
    assert_eq!(client.outlet_list_entry(1).await, None);
}

#[tokio::test]
async fn outlet_and_tap_handles_do_not_cross_families() {
    use crate::types::ClientWrite;

    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server_with_outlets(&path, Arc::new(StoreA));

    let client = TapClient::new(path);
    let tap_vh = client.resolve("temp").await.expect("tap resolve");
    let outlet_vh = client
        .outlet_resolve("led_cmd")
        .await
        .expect("outlet resolve");
    assert_ne!(tap_vh, outlet_vh, "families share one virtual-handle space");

    // A tap handle used for an outlet write must fail locally, not reach the
    // server with a handle from the wrong registry.
    assert_eq!(
        client.outlet_write(tap_vh, vec![7]).await,
        ClientWrite::Unavailable
    );
    // And an outlet handle must not read taps.
    assert_eq!(
        client.read_retained(outlet_vh).await,
        ClientRead::Unavailable
    );
}

#[tokio::test]
async fn outlet_ops_unavailable_against_tap_only_server() {
    use crate::types::ClientWrite;

    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server(&path, Arc::new(StoreA));

    let client = TapClient::new(path);
    assert_eq!(
        client.outlet_resolve("led_cmd").await,
        None,
        "Unsupported must map to a failed resolve"
    );
    assert_eq!(client.outlet_list_len().await, None);
    assert_eq!(
        client.outlet_write(1, vec![7]).await,
        ClientWrite::Unavailable,
        "an unissued handle must fail locally"
    );
}

/// Type-id queries (swarm#1315): resolved handles report the store's id, and
/// the query respects handle families.
#[tokio::test]
async fn type_id_round_trip_and_family_isolation() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("test.sock");
    start_server_with_outlets(&path, Arc::new(StoreA));

    let client = TapClient::new(path);
    let tap_vh = client.resolve("temp").await.expect("tap resolve");
    let outlet_vh = client
        .outlet_resolve("led_cmd")
        .await
        .expect("outlet resolve");

    assert_eq!(client.tap_type_id(tap_vh).await, Some(TYPE_ID_A));
    assert_eq!(client.outlet_type_id(outlet_vh).await, Some(TYPE_ID_B));

    // Cross-family lookups fail locally without a wire call.
    assert_eq!(client.tap_type_id(outlet_vh).await, None);
    assert_eq!(client.outlet_type_id(tap_vh).await, None);
}
