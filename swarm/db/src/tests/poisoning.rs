//! A failed write poisons fjall; the store must refuse writes loudly and
//! reopen itself once writes can land again.
//!
//! The failure is a real one: `RLIMIT_FSIZE` caps how far any file may grow,
//! so the journal append fails just as it would on a full disk. The limit is
//! process-wide, which nextest's process-per-test keeps to this test.

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use crate::domain::Scope;
use crate::store::{Options, Store, TransactionOptions, Unavailable};

use super::{read, write};

struct FileSizeLimit(libc::rlimit);

impl FileSizeLimit {
    fn set(bytes: u64) -> Self {
        // SAFETY: plain libc calls on a locally owned struct.
        unsafe {
            // Past the limit a write fails with EFBIG; by default it also kills the process.
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);

            let mut original = std::mem::zeroed::<libc::rlimit>();
            assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &raw mut original), 0);

            let limited = libc::rlimit {
                rlim_cur: bytes,
                rlim_max: original.rlim_max,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &raw const limited), 0);

            Self(original)
        }
    }
}

impl Drop for FileSizeLimit {
    fn drop(&mut self) {
        // SAFETY: restores the limit read in `set`.
        unsafe {
            libc::setrlimit(libc::RLIMIT_FSIZE, &raw const self.0);
        }
    }
}

fn open(dir: &std::path::Path) -> Store {
    Store::init(Options {
        directory: Some(dir.to_path_buf()),
        ..Options::test()
    })
    .expect("unable to open storage")
}

/// Writes past the file size limit until a commit fails; the store is
/// poisoned from then on.
fn fill_until_poisoned(store: &Store, scope: &Scope<'_>) {
    let mut failure = None;
    let mut value = [0u8; 4096];
    for i in 0..1024 {
        // Incompressible, so the journal grows by the full value.
        rand::fill(&mut value);
        let mut tx = write(store);
        tx.key_put(scope.kv(&format!("filler-{i}")), &value)
            .expect("unable to write");
        if let Err(err) = tx.commit() {
            failure = Some(err);
            break;
        }
    }
    // The commit that hits the failure reports the I/O error itself.
    failure.expect("a write past the file size limit must fail");
    assert!(store.is_unavailable());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_poisoned_store_refuses_writes_and_reopens() {
    let dir = tempfile::tempdir().expect("unable to create a temp dir");
    let store = open(dir.path());
    let scope = Scope::default();

    let mut tx = write(&store);
    tx.key_put(scope.kv("before"), b"kept")
        .expect("unable to write");
    tx.commit().expect("a healthy store commits");

    // Holds the old database open; the reopen has to abort it.
    let remote = store
        .begin_remote(&TransactionOptions::write())
        .expect("unable to start a remote tx");

    let limit = FileSizeLimit::set(1024 * 1024);
    fill_until_poisoned(&store, &scope);

    let mut tx = write(&store);
    tx.key_put(scope.kv("while-poisoned"), b"lost")
        .expect("unable to write");
    let err = tx
        .commit()
        .expect_err("a poisoned store refuses every write");
    assert!(err.is::<Unavailable>(), "got: {err:#}");

    drop(limit);

    let deadline = Instant::now() + Duration::from_secs(30);
    while store.is_unavailable() {
        assert!(
            Instant::now() < deadline,
            "the store never reopened after writes could land again"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        store.find_remote_tx(remote).is_none(),
        "a remote transaction cannot outlive the database it was opened on"
    );

    let mut tx = write(&store);
    tx.key_put(scope.kv("after"), b"landed")
        .expect("unable to write");
    tx.commit().expect("a reopened store commits");

    let mut tx = read(&store);
    for key in ["before", "after"] {
        assert!(
            tx.key_get(scope.kv(key)).expect("unable to read").is_some(),
            "'{key}' must survive the reopen"
        );
    }
    assert!(
        tx.key_get(scope.kv("while-poisoned"))
            .expect("unable to read")
            .is_none(),
        "a refused write must not appear"
    );
}

/// A reopen that fails leaves no database at all, not even one that serves
/// reads, so it is retried promptly rather than on the flap backoff.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_reopen_is_retried_promptly() {
    // SAFETY: plain libc call.
    if unsafe { libc::geteuid() } == 0 {
        // Root ignores directory permissions, which is what makes the reopen fail.
        return;
    }

    let dir = tempfile::tempdir().expect("unable to create a temp dir");
    let store = open(dir.path());
    let scope = Scope::default();

    let mut tx = write(&store);
    tx.key_put(scope.kv("before"), b"kept")
        .expect("unable to write");
    tx.commit().expect("a healthy store commits");

    // Without search permission on the directory the lock file can't be
    // opened, so every reopen fails. The open database keeps writing through
    // the descriptors it already holds.
    let permissions = std::fs::metadata(dir.path())
        .expect("unable to stat the data directory")
        .permissions();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o000))
        .expect("unable to lock the data directory");

    let limit = FileSizeLimit::set(1024 * 1024);
    fill_until_poisoned(&store, &scope);
    drop(limit);

    // The failed reopen left nothing behind: reads stop working too.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match store.begin_local(&TransactionOptions::read()) {
            Ok(_) => {
                assert!(Instant::now() < deadline, "no reopen was attempted");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(err) => {
                assert!(err.is::<Unavailable>(), "got: {err:#}");
                break;
            }
        }
    }

    // Long enough that a backoff doubling on every failure would put the next
    // attempt well after the directory is usable again.
    tokio::time::sleep(Duration::from_secs(7)).await;
    std::fs::set_permissions(dir.path(), permissions).expect("unable to unlock the data directory");

    let deadline = Instant::now() + Duration::from_secs(3);
    while store.is_unavailable() {
        assert!(
            Instant::now() < deadline,
            "the store did not reopen promptly once it could"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let mut tx = read(&store);
    assert!(
        tx.key_get(scope.kv("before"))
            .expect("unable to read")
            .is_some(),
        "'before' must survive the reopen"
    );
}
