//! What runtimes export of their telemetry to files: a runtime whose telemetry config names a
//! `file_export_dir` writes its spans and the latest values of its metrics there, as JSON lines
//! (`swarm_telemetry::file`). A test sets that config with [`crate::myrmic::StoppedRt::telemetry`],
//! and [`Files`] reads the files back from the runtimes' hosts, merged.
//!
//! A runtime writes its metrics every `metrics.export_interval_ms` (the `OTel` default is a
//! minute), so a test that reads a metric sets a short interval and waits one interval after what
//! it measures.
//!
//! # Replication messages per scope
//!
//! A runtime counts the replication messages it receives in `repl_msgs_recv` (and their bytes in
//! `repl_msg_bytes_recv`), with two attributes: `msg`, the kind (`ANNOUNCE`, `CHANGESET`, `PROBE`,
//! `CHANGESET_REQ`), and `subject`, what the replicator that received it replicates, written like
//! its key expressions: `<namespace>/<database>/<schema>`, with `*` for the levels it spans. So
//! the count is per scope for a scope replicated on its own, and per namespace (`sys/*/*`) for a
//! namespace replicated as a whole.
//!
//! ```ignore
//! use test_framework::telemetry;
//!
//! const DIR: &str = "/tmp/myrmic-telemetry";
//! let config = swarm_telemetry::TelemetryConfig {
//!     file_export_dir: Some(DIR.into()),
//!     metrics: swarm_telemetry::config::metrics::Config {
//!         export_interval_ms: Some(500),
//!         ..Default::default()
//!     },
//!     ..Default::default()
//! };
//! let a = swarm.add(a_host.myrmic(&group).runtime().telemetry(config));
//! start_runtimes!(swarm; a);
//! // ... replicate the scope `shop/orders/public` onto `a` and write to it ...
//!
//! let files = telemetry::Files::new(vec![a_host], DIR.to_owned());
//! let received = files
//!     .get_counter("repl_msgs_recv", &[("subject", "shop/orders/public")])
//!     .await;
//! ```

use serde::{Serialize, de::DeserializeOwned};
use swarm_telemetry::db::opentelemetry_proto::tonic::{
    common::v1::{KeyValue, any_value},
    metrics::v1::{Metric, metric, number_data_point},
    trace::v1::Span,
};
use swarm_telemetry::export::ScopedEntry;
use swarm_telemetry::file::{FILE_METRICS_LATEST, FILE_TRACES};

use crate::host::Remote;

/// The telemetry files of the runtimes on some hosts: which hosts to read from, and the
/// `file_export_dir` their runtimes were configured with. Two runtimes on one host need two
/// directories: each rewrites its metrics file whole, so in a shared one only the last writer's
/// metrics would be there.
pub struct Files<H> {
    hosts: Vec<H>,
    dir: String,
}

impl<H: Remote> Files<H> {
    /// the files the runtimes on `hosts` export to `dir`
    pub fn new(hosts: Vec<H>, dir: String) -> Self {
        Self { hosts, dir }
    }

    /// Every span every host exported, merged. Reads all files again on each call.
    pub async fn get_spans(&self) -> Vec<Span> {
        self.fetch_entries(FILE_TRACES).await
    }

    /// The latest value of every metric on every host, merged: one entry per metric and
    /// exporting process. Reads all files again on each call.
    pub async fn get_latest_metrics(&self) -> Vec<Metric> {
        self.fetch_entries(FILE_METRICS_LATEST).await
    }

    /// The latest value of the integer counter `name`, summed over the data points of every host
    /// and process that carry all of `attributes` (key and string value). 0 if none has counted
    /// it yet: a counter only shows up in the files once it counted something.
    pub async fn get_counter(&self, name: &str, attributes: &[(&str, &str)]) -> u64 {
        sum_counter(&self.get_latest_metrics().await, name, attributes)
    }

    /// Fetches `file` from every host at once and decodes its [`ScopedEntry`] JSON lines.
    async fn fetch_entries<T>(&self, file: &str) -> Vec<T>
    where
        T: Serialize + DeserializeOwned + std::fmt::Debug,
    {
        let path = format!("{}/{file}", self.dir);
        let fetches = self.hosts.iter().map(|host| fetch_lines(host, &path));
        futures::future::join_all(fetches)
            .await
            .into_iter()
            .flat_map(|lines| decode_lines(&path, &lines))
            .collect()
    }
}

/// The lines of the file at `path` on `host`. A missing file has none: a host whose runtimes
/// exported nothing yet adds no entries. Panics if the file can't be read.
async fn fetch_lines(host: &impl Remote, path: &str) -> Vec<String> {
    // `$1`: the file
    let read = r#"[ ! -e "$1" ] || cat "$1""#;
    let output = tokio::process::Command::from(host.command("sh", &["-c", read, "sh", path]))
        .output()
        .await
        .unwrap_or_else(|err| panic!("failed to run the read of {path}: {err}"));
    assert!(
        output.status.success(),
        "reading {path} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The entries in `lines`, the lines of the file at `path`. Its last line may be torn, as the
/// exporter may be appending to the file while it's read; that line is left out. Panics if any
/// other line isn't an entry.
fn decode_lines<T>(path: &str, lines: &[String]) -> Vec<T>
where
    T: Serialize + DeserializeOwned + std::fmt::Debug,
{
    let last = lines.len().saturating_sub(1);
    lines
        .iter()
        .enumerate()
        .filter_map(
            |(index, line)| match serde_json::from_str::<ScopedEntry<T>>(line) {
                Ok(entry) => Some(entry.data),
                Err(_) if index == last => None,
                Err(err) => panic!("line {} of {path} is no telemetry entry: {err}", index + 1),
            },
        )
        .collect()
}

/// [`Files::get_counter`] over `metrics`
fn sum_counter(metrics: &[Metric], name: &str, attributes: &[(&str, &str)]) -> u64 {
    metrics
        .iter()
        .filter(|metric| metric.name == name)
        .flat_map(|metric| match &metric.data {
            Some(metric::Data::Sum(sum)) => sum.data_points.iter(),
            other => panic!("`{name}` is no counter: {other:?}"),
        })
        .filter(|point| {
            attributes
                .iter()
                .all(|(key, value)| get_string_attribute(&point.attributes, key) == Some(*value))
        })
        .map(|point| match point.value {
            Some(number_data_point::Value::AsInt(value)) => u64::try_from(value)
                .unwrap_or_else(|_| panic!("the counter `{name}` is negative: {value}")),
            other => panic!("the counter `{name}` has no integer value: {other:?}"),
        })
        .sum()
}

/// the string value of the attribute `key` among `attributes`, if it has one
fn get_string_attribute<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.key == key)
        .and_then(|attribute| attribute.value.as_ref()?.value.as_ref())
        .and_then(|value| match value {
            any_value::Value::StringValue(value) => Some(value.as_str()),
            // a value of another type is no string to match
            _ => None,
        })
}
