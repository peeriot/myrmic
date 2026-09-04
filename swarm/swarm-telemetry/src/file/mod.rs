//! File-backed telemetry export: every signal is written to local JSON-lines
//! files instead of the shared db, for deployments where telemetry must not
//! compete with the workload for the datalayer — a load benchmark most of all.
//! The rack harness fetches the files over SSH after a pass
//! (`test-framework`'s `telemetry_files`) and serves the same queries the
//! db-backed tables used to answer.
//!
//! Layout under the configured directory (`telemetry.file_export_dir`):
//! - [`FILE_TRACES`] / [`FILE_LOGS`] — append-only, one
//!   [`ScopedEntry`]-encoded span/log record per line.
//! - [`FILE_METRICS_LATEST`] — the current cumulative value of every metric,
//!   one per line, rewritten atomically (tmp + rename) on each periodic
//!   export: the file equivalent of the db's `metrics_latest` table.
//!
//! A local file write cannot be broken by db discovery, so `force_flush` on
//! these exporters is cheap and reliable — the flakiness class where a
//! benchmark dies because one host's telemetry export lost its db route does
//! not exist on this path.

use std::io::Write as _;
use std::path::PathBuf;

pub use opentelemetry_proto;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use serde::Serialize;

pub use crate::export::ScopedEntry;

mod logs;
mod metrics;
mod trace;

pub const FILE_TRACES: &str = "traces.jsonl";
pub const FILE_LOGS: &str = "logs.jsonl";
pub const FILE_METRICS_LATEST: &str = "metrics-latest.jsonl";

#[derive(Debug, Clone)]
pub struct FileExporter {
    dir: PathBuf,
}

impl FileExporter {
    /// Opens (creating if needed) `dir` for telemetry export.
    pub fn new(dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Appends one [`ScopedEntry`] JSON line per entry to `file`. The whole
    /// batch is a single `write` on an `O_APPEND` handle, so concurrent
    /// exporters (or a reader `cat`ing the file mid-run) see whole batches,
    /// not interleaved fragments — at worst a torn final line, which readers
    /// skip like any undecodable row.
    fn append_lines<T>(&self, file: &str, entries: Vec<(Option<String>, T)>) -> OTelSdkResult
    where
        T: Serialize + std::fmt::Debug,
    {
        let count = entries.len();
        let result = self.write_lines(file, entries, false);
        crate::record_insert_batch_outcome(file, count, None, result.as_ref());
        result.map_err(|err| OTelSdkError::InternalFailure(err.to_string()))
    }

    /// Replaces `file` with one [`ScopedEntry`] JSON line per entry, via a
    /// tmp-file rename so a concurrent reader never sees a partial file.
    fn rewrite_lines<T>(&self, file: &str, entries: Vec<(Option<String>, T)>) -> OTelSdkResult
    where
        T: Serialize + std::fmt::Debug,
    {
        let count = entries.len();
        let result = self.write_lines(file, entries, true);
        crate::record_insert_batch_outcome(file, count, None, result.as_ref());
        result.map_err(|err| OTelSdkError::InternalFailure(err.to_string()))
    }

    fn write_lines<T>(
        &self,
        file: &str,
        entries: Vec<(Option<String>, T)>,
        replace: bool,
    ) -> anyhow::Result<()>
    where
        T: Serialize + std::fmt::Debug,
    {
        let mut buffer = Vec::new();
        for (scope_name, data) in entries {
            let entry = ScopedEntry { scope_name, data };
            serde_json::to_writer(&mut buffer, &entry)?;
            buffer.push(b'\n');
        }

        let path = self.dir.join(file);
        if replace {
            // A temp name no other writer can be using, which is what makes the
            // rename atomic *per writer*. A fixed `{file}.tmp` is shared by
            // everything exporting into the directory — two runtimes configured
            // with the same one, or a reader overlapping its own periodic
            // export — and since `write` truncates and refills, one writer can
            // rename another's half-written buffer into place.
            static NEXT_TMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = NEXT_TMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            let tmp = self
                .dir
                .join(format!("{file}.{}.{seq:x}.tmp", std::process::id()));
            std::fs::write(&tmp, &buffer)?;
            std::fs::rename(&tmp, &path)?;
        } else {
            let mut handle = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            handle.write_all(&buffer)?;
        }

        Ok(())
    }
}
