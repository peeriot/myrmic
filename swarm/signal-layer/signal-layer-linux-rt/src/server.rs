//! Runtime bootstrap: bind the tap server socket with correct permissions,
//! delegate to `signal_layer_ipc::serve`, and unlink the socket once the
//! server is gone.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use signal_layer_ipc::{OutletStore, TapStore};

/// The bound socket's file on disk, unlinked on drop so that however the
/// server ends (returning, failing part-way through setup, or its future
/// being dropped) no stale path is left behind.
struct SocketFile {
    path: PathBuf,
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        // Nothing to report a failure to from here, and a path that is
        // already gone is the state being established anyway.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// [`run_signal_server`] without an outlet store: outlet requests answer
/// `Unsupported`. The entry-point used by generated sensors-only pipelines;
/// kept under its original name (SR-16).
pub async fn run_tap_server(path: PathBuf, store: Arc<dyn TapStore>) -> io::Result<()> {
    run_signal_server(path, store, None).await
}

/// Bind a Unix-domain socket at `path`, set its mode to `0o660`, remove any
/// stale socket file first, and delegate to [`signal_layer_ipc::serve`] with
/// the tap store and (when present) the outlet store.
///
/// This is the entry-point used by generated Linux pipeline binaries (SR-16).
///
/// The socket file is unlinked once the server is gone, including when the
/// returned future is dropped, so cancelling the task is the orderly
/// shutdown. A crash still leaves the file; the next start removes it before
/// binding.
///
/// Security properties (S2 + S3):
/// - Stale-socket removal is unconditional (ignores `NotFound`) to avoid the
///   TOCTOU race in `if exists() { remove_file }` (S3).
/// - After bind, `chmod(2)` sets the mode to 0o660 immediately (S2).  The
///   bind→chmod window is minimised by the fact that `signal_layer_ipc::serve`
///   does not start accepting connections until after this function returns.
///   Note: `fchmod(2)` on a Unix-socket FD does not update the filesystem inode
///   mode on Linux, so `chmod`-by-path is used here.
pub async fn run_signal_server(
    path: PathBuf,
    store: Arc<dyn TapStore>,
    outlets: Option<Arc<dyn OutletStore>>,
) -> io::Result<()> {
    // S3: remove unconditionally, ignoring NotFound — avoids the TOCTOU race
    // between `path.exists()` and `remove_file(&path)`.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    let listener = tokio::net::UnixListener::bind(&path)?;
    let socket_file = SocketFile { path };

    // S2: set socket permissions to 0o660 immediately after bind.
    let path_cstr = std::ffi::CString::new(
        socket_file
            .path
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 socket path"))?,
    )
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL byte in socket path"))?;

    // SAFETY: `path_cstr` is a valid NUL-terminated C string for the duration
    // of this call; `libc::chmod` does not dereference any other Rust memory.
    let rc = unsafe { libc::chmod(path_cstr.as_ptr(), 0o660) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    signal_layer_ipc::serve(listener, store, outlets).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use signal_layer_ipc::{StoreRead, TapStore};
    use tempfile::TempDir;

    use super::*;

    // ── Minimal TapStore stub for tests ───────────────────────────────────

    struct OneTapStore {
        name: &'static str,
        handle: u32,
        retained_bytes: Vec<u8>,
    }

    impl TapStore for OneTapStore {
        fn resolve(&self, name: &str) -> Option<u32> {
            if name == self.name {
                Some(self.handle)
            } else {
                None
            }
        }

        fn read_retained(&self, h: u32) -> StoreRead {
            if h == self.handle {
                StoreRead::Value {
                    timestamp_ms: 42,
                    bytes: self.retained_bytes.clone(),
                }
            } else {
                StoreRead::InvalidHandle
            }
        }

        fn take_event(&self, h: u32) -> StoreRead {
            if h == self.handle {
                StoreRead::Empty
            } else {
                StoreRead::InvalidHandle
            }
        }

        fn list_len(&self) -> u32 {
            1
        }

        fn list_entry(&self, index: u32) -> Option<(String, u8)> {
            if index == 0 {
                Some((self.name.to_string(), 0))
            } else {
                None
            }
        }

        fn type_id(&self, h: u32) -> Option<u32> {
            (h == self.handle).then_some(0xF32)
        }
    }

    fn make_store() -> Arc<dyn TapStore> {
        Arc::new(OneTapStore {
            name: "temperature",
            handle: 1,
            retained_bytes: vec![1, 2, 3],
        })
    }

    /// Binds the server, sends a `TapResolve` request and gets a `Handle` back.
    /// Also checks that the socket file mode is 0o660.
    #[tokio::test]
    async fn run_tap_server_serves_resolve_and_sets_mode_0660() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("test.sock");

        let store = make_store();
        let path_clone = socket_path.clone();
        let server_handle = tokio::spawn(async move {
            run_tap_server(path_clone, store).await.unwrap();
        });

        // Give the server a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Check socket mode.
        check_socket_mode(&socket_path, 0o660);

        // Connect and send a TapResolve.
        let resp = do_tap_resolve(&socket_path, "temperature").await;
        assert_eq!(resp, signal_layer_ipc::Response::Handle { handle: 1 });

        server_handle.abort();
    }

    /// A stale socket file must not prevent re-binding.
    #[tokio::test]
    async fn run_tap_server_removes_stale_socket_file() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("stale.sock");

        // Create a stale file at the socket path.
        std::fs::write(&socket_path, b"stale").unwrap();
        assert!(socket_path.exists(), "stale file should exist");

        let store = make_store();
        let path_clone = socket_path.clone();
        let server_handle = tokio::spawn(async move {
            run_tap_server(path_clone, store).await.unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Server must have started — socket mode is 0o660.
        check_socket_mode(&socket_path, 0o660);

        // Issue a request to confirm the server is live.
        let resp = do_tap_resolve(&socket_path, "temperature").await;
        assert_eq!(resp, signal_layer_ipc::Response::Handle { handle: 1 });

        server_handle.abort();
    }

    /// Stopping the server unlinks its socket file.
    #[tokio::test]
    async fn aborting_the_server_removes_the_socket_file() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("abort.sock");

        let server_handle =
            tokio::spawn(run_signal_server(socket_path.clone(), make_store(), None));

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(socket_path.exists(), "server should have bound the socket");

        server_handle.abort();
        let _ = server_handle.await;

        assert!(
            !socket_path.exists(),
            "socket file should be removed once the server is gone"
        );
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    fn check_socket_mode(path: &std::path::Path, expected_mode: u32) {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).expect("metadata");
        let mode = meta.mode() & 0o777;
        assert_eq!(
            mode, expected_mode,
            "socket mode should be {expected_mode:o}, got {mode:o}"
        );
    }

    async fn do_tap_resolve(
        socket_path: &std::path::Path,
        name: &str,
    ) -> signal_layer_ipc::Response {
        use tokio::net::UnixStream;

        let mut stream = UnixStream::connect(socket_path).await.unwrap();

        // Send Hello.
        let hello = signal_layer_ipc::Request::Hello {
            protocol_version: signal_layer_ipc::PROTOCOL_VERSION,
        };
        signal_layer_ipc::write_frame(&mut stream, &hello)
            .await
            .unwrap();

        // Read HelloOk.
        let frame = signal_layer_ipc::read_frame(&mut stream).await.unwrap();
        let _: signal_layer_ipc::Response = postcard::from_bytes(&frame).unwrap();

        // Send TapResolve.
        let req = signal_layer_ipc::Request::TapResolve {
            name: name.to_string(),
        };
        signal_layer_ipc::write_frame(&mut stream, &req)
            .await
            .unwrap();

        // Read response.
        let frame = signal_layer_ipc::read_frame(&mut stream).await.unwrap();
        postcard::from_bytes(&frame).unwrap()
    }
}
