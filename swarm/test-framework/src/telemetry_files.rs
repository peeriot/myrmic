//! Fetch-side of file-backed telemetry export (`swarm_telemetry::file`): when
//! a rack deployment writes each host's telemetry to local JSON-lines files
//! instead of the shared db (so telemetry never competes with the workload for
//! the datalayer — see `rack::RackTelemetry::Files`), this module pulls those
//! files over SSH and serves the merged view the db-backed telemetry tables
//! used to provide.

use serde::Serialize;
use serde::de::DeserializeOwned;
use swarm_telemetry::db::opentelemetry_proto::tonic::logs::v1::LogRecord;
use swarm_telemetry::db::opentelemetry_proto::tonic::metrics::v1::Metric;
use swarm_telemetry::db::opentelemetry_proto::tonic::trace::v1::Span;
use swarm_telemetry::export::ScopedEntry;
use swarm_telemetry::file::{FILE_LOGS, FILE_METRICS_LATEST, FILE_TRACES};

/// Where a provisioned deployment's telemetry files live: which hosts to fetch
/// from, and the `file_export_dir` their runtimes were configured with.
#[derive(Debug, Clone)]
pub struct TelemetryFiles {
    hosts: Vec<String>,
    dir: String,
}

impl TelemetryFiles {
    #[must_use]
    pub fn new(hosts: Vec<String>, dir: String) -> Self {
        Self { hosts, dir }
    }

    /// Every span exported by every host, merged. One full fetch per call —
    /// same cost model as the db path's full trace-table scan per query.
    pub async fn spans(&self) -> Vec<Span> {
        self.fetch_entries(FILE_TRACES).await
    }

    /// Every log record exported by every host, merged.
    pub async fn logs(&self) -> Vec<LogRecord> {
        self.fetch_entries(FILE_LOGS).await
    }

    /// The latest cumulative value of every metric on every host, merged — the
    /// file equivalent of the db's `metrics_latest` table (which likewise held
    /// one row per metric per exporting process).
    pub async fn latest_metrics(&self) -> Vec<Metric> {
        self.fetch_entries(FILE_METRICS_LATEST).await
    }

    /// Fetches `file` from every host concurrently and decodes its
    /// [`ScopedEntry`] JSON lines, dropping lines that fail to decode (at most
    /// a torn final line mid-write — the file exporter appends whole batches).
    async fn fetch_entries<T>(&self, file: &str) -> Vec<T>
    where
        T: Serialize + DeserializeOwned + std::fmt::Debug,
    {
        let fetches = self
            .hosts
            .iter()
            .map(|host| fetch_lines(host, format!("{}/{file}", self.dir)));

        futures::future::join_all(fetches)
            .await
            .into_iter()
            .flatten()
            .filter_map(|line| {
                // At most a torn final line mid-write; anything more deserves
                // eyes, but must not sink the report that is being assembled.
                serde_json::from_str::<ScopedEntry<T>>(&line)
                    .inspect_err(|err| {
                        eprintln!("warning: skipping undecodable telemetry file line: {err}");
                    })
                    .ok()
            })
            .map(|entry| entry.data)
            .collect()
    }
}

/// `cat`s `path` on `host` over SSH, returning its lines. A missing file is
/// empty, not an error — a host whose runtime exported nothing yet (or was
/// torn down) simply contributes no entries; a failed connection warns, since
/// silently missing a whole host's telemetry is exactly the kind of gap a
/// benchmark report must not paper over.
async fn fetch_lines(host: &str, path: String) -> Vec<String> {
    let mut command = tokio::process::Command::new("ssh");
    command.arg("-o").arg("BatchMode=yes");
    if let Some(identity_file) = crate::ssh_identity_file() {
        command.arg("-i").arg(identity_file);
    }
    if let Some(known_hosts_file) = crate::ssh_known_hosts_file() {
        command
            .arg("-o")
            .arg(format!("UserKnownHostsFile={known_hosts_file}"));
    }

    // The remote shell resolves the redirect: a missing file is an empty
    // result by design, while ssh-level failures still exit non-zero.
    let output = command
        .arg(host)
        .arg(format!("cat {path} 2>/dev/null || true"))
        .output()
        .await;

    let output = match output {
        Ok(output) => output,
        Err(err) => {
            eprintln!("warning: unable to run ssh to fetch telemetry from {host}: {err}");
            return Vec::new();
        }
    };

    if !output.status.success() {
        eprintln!(
            "warning: fetching telemetry file {path} from {host} failed (exit {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}
