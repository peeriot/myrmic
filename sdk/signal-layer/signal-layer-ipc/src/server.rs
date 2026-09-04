//! IPC tap server: accept-loop + per-connection request handling.

use std::sync::Arc;
use tokio::net::UnixListener;

use crate::{OutletStore, TapStore};

/// Maximum number of concurrently open connections (S1: slow-loris / FD cap).
const MAX_CONNECTIONS: usize = 64;

/// Timeout for the initial Hello handshake (S1: slow-loris mitigation).
const HANDSHAKE_TIMEOUT_SECS: u64 = 5;

/// Per-request idle read timeout (S1: slow-loris mitigation for post-handshake).
/// A client that completes the Hello then stalls mid-frame would otherwise hold
/// a [`MAX_CONNECTIONS`] permit indefinitely.  Larger than the handshake timeout
/// to give legitimate clients time to issue their first request.
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Accept connections on `listener` and serve tap requests from `store`,
/// plus outlet requests from `outlets` when one is provided (outlet requests
/// answer `Unsupported` otherwise, preserving the sensors-only v1 behaviour).
///
/// Each connection goes through the version handshake; any frame error
/// drops the connection (fail-closed).  This function runs until the
/// listener is closed or an unrecoverable error occurs.
///
/// At most [`MAX_CONNECTIONS`] concurrent connections are serviced; additional
/// connection attempts are accepted and immediately dropped when the cap is
/// reached (S1: no unbounded spawn growth / FD exhaustion).  A per-connection
/// handshake timeout of [`HANDSHAKE_TIMEOUT_SECS`] seconds defends against
/// slow-loris attacks.
pub async fn serve(
    listener: UnixListener,
    store: Arc<dyn TapStore>,
    outlets: Option<Arc<dyn OutletStore>>,
) -> std::io::Result<()> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));

    loop {
        let (stream, _addr) = listener.accept().await?;

        // Try to acquire a slot without blocking the accept loop.
        match Arc::clone(&semaphore).try_acquire_owned() {
            Ok(permit) => {
                let store = Arc::clone(&store);
                let outlets = outlets.clone();
                tokio::spawn(async move {
                    let _permit = permit; // dropped when connection task ends
                    if let Err(_e) = handle_connection(stream, store, outlets).await {
                        // Errors are expected (client disconnect, bad frame); drop silently.
                    }
                });
            }
            Err(_) => {
                // Cap reached: drop the connection immediately (fail-closed).
                drop(stream);
            }
        }
    }
}

async fn handle_connection(
    mut stream: tokio::net::UnixStream,
    store: Arc<dyn TapStore>,
    outlets: Option<Arc<dyn OutletStore>>,
) -> Result<(), crate::framing::FrameError> {
    use tokio::io::AsyncWriteExt;

    use crate::PROTOCOL_VERSION;
    use crate::framing::{decode_frame, read_frame, write_frame};
    use crate::types::{Request, Response};

    // ── Handshake ─────────────────────────────────────────────────────────
    // Apply a timeout to the Hello read to defend against slow-loris (S1).
    let (mut reader, mut writer) = stream.split();
    let frame = tokio::time::timeout(
        tokio::time::Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        read_frame(&mut reader),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "handshake timeout"))??;
    let req: Request = decode_frame(&frame)?;

    match req {
        Request::Hello { protocol_version } => {
            if protocol_version == PROTOCOL_VERSION {
                write_frame(
                    &mut writer,
                    &Response::HelloOk {
                        version: PROTOCOL_VERSION,
                    },
                )
                .await?;
            } else {
                write_frame(
                    &mut writer,
                    &Response::HelloRejected {
                        supported_version: PROTOCOL_VERSION,
                    },
                )
                .await?;
                // Flush and close.
                writer.flush().await?;
                // Drop — connection closes on return.
                return Ok(());
            }
        }
        _ => {
            // Non-Hello first message is a protocol error; drop the connection.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "expected Hello as first message",
            )
            .into());
        }
    }

    // ── Request loop ──────────────────────────────────────────────────────
    // Apply a per-request idle read timeout (S1): a client that completes the
    // Hello but then stalls mid-frame would otherwise hold a permit indefinitely.
    loop {
        let frame = tokio::time::timeout(
            tokio::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
            read_frame(&mut reader),
        )
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "request read timeout"))??;
        let req: Request = decode_frame(&frame)?;

        let resp = dispatch_request(req, &*store, outlets.as_deref());
        write_frame(&mut writer, &resp).await?;
    }
}

fn dispatch_request(
    req: crate::types::Request,
    store: &dyn TapStore,
    outlets: Option<&dyn OutletStore>,
) -> crate::types::Response {
    use crate::types::{Request, Response};

    match req {
        Request::Hello { .. } => {
            // Hello mid-stream is a protocol error; respond InvalidHandle to
            // signal the confusion and drop on the next frame error.
            Response::InvalidHandle
        }
        Request::TapResolve { name } => match store.resolve(&name) {
            Some(h) => Response::Handle { handle: h },
            None => Response::NotFound,
        },
        Request::TapReadRetained { handle } => {
            retained_read_to_response(store.read_retained(handle))
        }
        Request::TapTakeEvent { handle } => event_read_to_response(store.take_event(handle)),
        Request::TapDrainBatch { handle: _ } => Response::Empty, // D1: always Empty
        Request::TapListLen => Response::Count {
            count: store.list_len(),
        },
        Request::TapListEntry { index } => match store.list_entry(index) {
            Some((name, kind)) => Response::Entry { name, kind },
            None => Response::OutOfRange,
        },
        // Outlet operations: served when an outlet store is present, otherwise
        // Unsupported (the sensors-only v1 answer, D10).
        Request::OutletResolve { name } => match outlets {
            Some(o) => match o.resolve(&name) {
                Some(h) => Response::Handle { handle: h },
                None => Response::NotFound,
            },
            None => Response::Unsupported,
        },
        Request::OutletWrite { handle, bytes } => match outlets {
            Some(o) => match o.write(handle, &bytes) {
                crate::types::StoreWrite::Ok => Response::Written,
                crate::types::StoreWrite::Rejected => Response::Rejected,
                crate::types::StoreWrite::InvalidHandle => Response::InvalidHandle,
            },
            None => Response::Unsupported,
        },
        Request::OutletListLen => match outlets {
            Some(o) => Response::Count {
                count: o.list_len(),
            },
            None => Response::Unsupported,
        },
        Request::OutletListEntry { index } => match outlets {
            Some(o) => match o.list_entry(index) {
                Some((name, kind)) => Response::Entry { name, kind },
                None => Response::OutOfRange,
            },
            None => Response::Unsupported,
        },
        Request::TapTypeId { handle } => match store.type_id(handle) {
            Some(id) => Response::TypeId { id },
            None => Response::InvalidHandle,
        },
        Request::OutletTypeId { handle } => match outlets {
            Some(o) => match o.type_id(handle) {
                Some(id) => Response::TypeId { id },
                None => Response::InvalidHandle,
            },
            None => Response::Unsupported,
        },
    }
}

/// Map a retained read to the wire response (`Retained` on success).
fn retained_read_to_response(read: crate::types::StoreRead) -> crate::types::Response {
    use crate::types::{Response, StoreRead};
    match read {
        StoreRead::Value {
            timestamp_ms,
            bytes,
        } => Response::Retained {
            timestamp_ms,
            bytes,
        },
        StoreRead::Empty => Response::Empty,
        StoreRead::InvalidHandle => Response::InvalidHandle,
    }
}

/// Map an event read to the wire response (`Event` on success).
fn event_read_to_response(read: crate::types::StoreRead) -> crate::types::Response {
    use crate::types::{Response, StoreRead};
    match read {
        StoreRead::Value { bytes, .. } => Response::Event { bytes },
        StoreRead::Empty => Response::Empty,
        StoreRead::InvalidHandle => Response::InvalidHandle,
    }
}
