//! IPC protocol types, framing, and versioned tap-server/client for Linux signal-layer.

mod framing;
mod path;
mod types;

pub mod client;
pub mod server;

#[cfg(test)]
mod client_tests;
#[cfg(test)]
mod server_tests;

pub use framing::{FrameError, read_frame, write_frame};
pub use path::default_socket_path;
pub use types::{
    ClientRead, ClientWrite, OutletStore, Request, Response, StoreRead, StoreWrite, TapStore,
};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_LEN: u32 = 64 * 1024;

/// Bytes a postcard varint-encoded `u32` occupies in the worst case.
const MAX_VARINT_U32_LEN: u32 = 5;

/// Worst-case postcard overhead of a request carrying a single variable-length
/// field: the enum-discriminant varint plus the field-length varint.
const REQUEST_ENVELOPE_LEN: u32 = 2 * MAX_VARINT_U32_LEN;

/// Longest name, in bytes, that a resolve request can carry and still fit
/// inside [`MAX_FRAME_LEN`].
///
/// Callers must reject longer names before allocating for them or writing them
/// to a connection. An oversized frame is refused by the peer's framer, which
/// costs the whole connection and every handle issued on it — not just the
/// offending call.
pub const MAX_RESOLVE_NAME_LEN: usize = (MAX_FRAME_LEN - REQUEST_ENVELOPE_LEN) as usize;

/// Longest payload, in bytes, that an outlet write can carry and still fit
/// inside [`MAX_FRAME_LEN`] — an `OutletWrite` envelope additionally carries
/// the handle varint. Same rationale as [`MAX_RESOLVE_NAME_LEN`]: an oversized
/// frame costs the shared connection, not just the offending call.
pub const MAX_OUTLET_WRITE_LEN: usize =
    (MAX_FRAME_LEN - REQUEST_ENVELOPE_LEN - MAX_VARINT_U32_LEN) as usize;

pub use client::{RECONNECT_SLA_SECS, TAP_CALL_TIMEOUT, TapClient};
pub use server::serve;

#[cfg(test)]
mod tests {
    use super::*;

    // ── Round-trip tests: one per Request variant ─────────────────────────

    fn rt_request(req: &Request) {
        let bytes = postcard::to_allocvec(req).expect("serialize");
        let decoded: Request = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(*req, decoded);
    }

    fn rt_response(resp: &Response) {
        let bytes = postcard::to_allocvec(resp).expect("serialize");
        let decoded: Response = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(*resp, decoded);
    }

    #[test]
    fn round_trip_request_hello() {
        rt_request(&Request::Hello {
            protocol_version: PROTOCOL_VERSION,
        });
    }

    #[test]
    fn round_trip_request_tap_resolve() {
        rt_request(&Request::TapResolve {
            name: "temperature".into(),
        });
    }

    #[test]
    fn round_trip_request_tap_read_retained() {
        rt_request(&Request::TapReadRetained { handle: 1 });
    }

    #[test]
    fn round_trip_request_tap_take_event() {
        rt_request(&Request::TapTakeEvent { handle: 2 });
    }

    #[test]
    fn round_trip_request_tap_drain_batch() {
        rt_request(&Request::TapDrainBatch { handle: 3 });
    }

    #[test]
    fn round_trip_request_tap_list_len() {
        rt_request(&Request::TapListLen);
    }

    #[test]
    fn round_trip_request_tap_list_entry() {
        rt_request(&Request::TapListEntry { index: 0 });
    }

    #[test]
    fn round_trip_request_outlet_resolve() {
        rt_request(&Request::OutletResolve { name: "led".into() });
    }

    #[test]
    fn round_trip_request_outlet_write() {
        rt_request(&Request::OutletWrite {
            handle: 1,
            bytes: vec![0xAA, 0xBB],
        });
    }

    #[test]
    fn round_trip_request_outlet_list_len() {
        rt_request(&Request::OutletListLen);
    }

    #[test]
    fn round_trip_request_outlet_list_entry() {
        rt_request(&Request::OutletListEntry { index: 0 });
    }

    #[test]
    fn round_trip_request_tap_type_id() {
        rt_request(&Request::TapTypeId { handle: 1 });
    }

    #[test]
    fn round_trip_request_outlet_type_id() {
        rt_request(&Request::OutletTypeId { handle: 1 });
    }

    // ── Round-trip tests: one per Response variant ────────────────────────

    #[test]
    fn round_trip_response_hello_ok() {
        rt_response(&Response::HelloOk {
            version: PROTOCOL_VERSION,
        });
    }

    #[test]
    fn round_trip_response_hello_rejected() {
        rt_response(&Response::HelloRejected {
            supported_version: PROTOCOL_VERSION,
        });
    }

    #[test]
    fn round_trip_response_handle() {
        rt_response(&Response::Handle { handle: 42 });
    }

    #[test]
    fn round_trip_response_not_found() {
        rt_response(&Response::NotFound);
    }

    #[test]
    fn round_trip_response_retained() {
        rt_response(&Response::Retained {
            timestamp_ms: 1234,
            bytes: vec![1, 2, 3],
        });
    }

    #[test]
    fn round_trip_response_event() {
        rt_response(&Response::Event {
            bytes: vec![9, 8, 7],
        });
    }

    #[test]
    fn round_trip_response_empty() {
        rt_response(&Response::Empty);
    }

    #[test]
    fn round_trip_response_invalid_handle() {
        rt_response(&Response::InvalidHandle);
    }

    #[test]
    fn round_trip_response_count() {
        rt_response(&Response::Count { count: 5 });
    }

    #[test]
    fn round_trip_response_entry() {
        rt_response(&Response::Entry {
            name: "hum".into(),
            kind: 1,
        });
    }

    #[test]
    fn round_trip_response_out_of_range() {
        rt_response(&Response::OutOfRange);
    }

    #[test]
    fn round_trip_response_unsupported() {
        rt_response(&Response::Unsupported);
    }

    #[test]
    fn round_trip_response_written() {
        rt_response(&Response::Written);
    }

    #[test]
    fn round_trip_response_rejected() {
        rt_response(&Response::Rejected);
    }

    #[test]
    fn round_trip_response_type_id() {
        rt_response(&Response::TypeId { id: 0xF32 });
    }

    // ── Malformed input decode ────────────────────────────────────────────

    #[test]
    fn malformed_request_decode_fails() {
        let result = postcard::from_bytes::<Request>(&[0xFF; 8]);
        assert!(result.is_err(), "expected error on malformed input");
    }

    #[test]
    fn malformed_response_decode_fails() {
        let result = postcard::from_bytes::<Response>(&[0xFF; 8]);
        assert!(result.is_err(), "expected error on malformed input");
    }

    // ── Oversized frame ───────────────────────────────────────────────────

    #[tokio::test]
    async fn oversized_frame_returns_too_large() {
        use std::io::Cursor;
        // Construct a 5-byte prefix claiming MAX_FRAME_LEN + 1 bytes.
        let len = MAX_FRAME_LEN + 1;
        let mut buf = Vec::new();
        buf.extend_from_slice(&len.to_le_bytes()); // 4-byte LE length
        // No payload bytes needed — the framer must reject on the length alone.
        let mut reader = Cursor::new(buf);
        let result = read_frame(&mut reader).await;
        assert!(
            matches!(result, Err(FrameError::TooLarge(_))),
            "expected TooLarge, got {result:?}"
        );
    }

    // ── Resolve-name bound ────────────────────────────────────────────────
    //
    // `MAX_RESOLVE_NAME_LEN` is derived from `MAX_FRAME_LEN` minus a worst-case
    // postcard envelope.  These tests pin the derivation so a change to the
    // wire encoding cannot silently make the bound unsafe or pointlessly tight.

    #[test]
    fn resolve_name_at_bound_serializes_within_frame_cap() {
        let req = Request::TapResolve {
            name: "n".repeat(MAX_RESOLVE_NAME_LEN),
        };
        let payload = postcard::to_allocvec(&req).expect("serialize");
        let len = u32::try_from(payload.len()).expect("payload length fits u32");
        assert!(
            len <= MAX_FRAME_LEN,
            "a name at the bound must serialize within {MAX_FRAME_LEN} bytes, got {len}"
        );
    }

    #[test]
    fn resolve_name_bound_leaves_no_unusable_headroom() {
        // The bound must not be so conservative that a name one byte longer
        // would still have fit — that would reject names the protocol allows.
        let req = Request::TapResolve {
            name: "n".repeat(MAX_RESOLVE_NAME_LEN),
        };
        let payload = postcard::to_allocvec(&req).expect("serialize");
        let slack = MAX_FRAME_LEN - u32::try_from(payload.len()).expect("fits u32");
        assert!(
            slack < REQUEST_ENVELOPE_LEN,
            "bound wastes {slack} bytes of frame budget"
        );
    }

    // ── Truncated / older-version frame (spec §4, workflow G4 "older inputs fail closed") ──
    //
    // A frame whose length prefix claims N bytes but the stream ends with fewer
    // bytes is a truncated (older/partial) input.  The spec requires "fail closed":
    // the framer must return an error, not silently decode a shorter value.

    #[tokio::test]
    async fn truncated_frame_payload_fails_closed() {
        use std::io::Cursor;
        // Length prefix claims 16 bytes; stream contains only 4 payload bytes.
        let claimed_len: u32 = 16;
        let mut buf = Vec::new();
        buf.extend_from_slice(&claimed_len.to_le_bytes());
        buf.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]); // only 4 of 16 bytes
        let mut reader = Cursor::new(buf);
        let result = read_frame(&mut reader).await;
        assert!(
            result.is_err(),
            "truncated frame must return an error (fail closed), got Ok"
        );
        // Must NOT be TooLarge — that would indicate the wrong error path.
        assert!(
            !matches!(result, Err(FrameError::TooLarge(_))),
            "truncated frame must not be TooLarge — expected an I/O/EOF error"
        );
    }

    /// A stream that ends after only 2 of the 4 length-prefix bytes.
    /// Must fail closed (EOF while reading the prefix itself).
    #[tokio::test]
    async fn half_length_prefix_fails_closed() {
        use std::io::Cursor;
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x01, 0x00]); // only 2 bytes of the 4-byte prefix
        let mut reader = Cursor::new(buf);
        let result = read_frame(&mut reader).await;
        assert!(
            result.is_err(),
            "half length prefix must return an error (fail closed)"
        );
    }
}
