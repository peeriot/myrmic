//! Types shared by every telemetry exporter backend (db, file): the persisted
//! entry envelope and small time helpers.

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

/// The envelope every exported telemetry entry is persisted in, whatever the
/// backend: the `OTel` instrumentation-scope name alongside the proto-encoded
/// signal (span, log record, metric).
#[derive(Debug, Deserialize, Serialize)]
pub struct ScopedEntry<T>
where
    T: Serialize + Debug,
{
    pub scope_name: Option<String>,
    pub data: T,
}

#[cfg(feature = "export-db")]
#[expect(
    clippy::expect_used,
    reason = "duration_since only fails if UNIX_EPOCH is later than `time` argument"
)]
// use saturating_duration_since once stabilized and make the call directly where used, this is a
// free standing function only not to repeat the clippy expect
pub(crate) fn duration_since_unix_epoch(time: std::time::SystemTime) -> std::time::Duration {
    time.duration_since(std::time::UNIX_EPOCH)
        .expect("now() > UNIX_EPOCH")
}
