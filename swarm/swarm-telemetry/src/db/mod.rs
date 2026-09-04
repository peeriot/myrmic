use std::{fmt::Debug, sync::Arc};

use anyhow::Context;
use db_client::v1::models::{Scope, tb_insert_batched};
pub use opentelemetry_proto;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use serde::Serialize;
use tokio::sync::RwLock;

pub mod logs;
pub mod metrics;
pub mod trace;

/// Namespace holding cell-exported telemetry. Reserved by the WASM host so a
/// cell cannot forge or clobber telemetry; the guards match this literal.
pub const NAMESPACE_TELE: &str = "tele";
pub const DATABASE: &str = "telemetry";
pub const TABLE_LOGS: &str = "logs";
pub const TABLE_TRACES: &str = "traces";
pub const TABLE_METRICS: &str = "metrics";
pub const TABLE_METRICS_LATEST: &str = "metrics_latest";

#[derive(Debug, Clone)]
pub struct DbExporter {
    client: db_client::v1::Client,
    retention: Arc<RwLock<Option<std::time::Duration>>>,
}

impl DbExporter {
    #[must_use]
    pub fn new(client: db_client::v1::Client, retention: Option<std::time::Duration>) -> Self {
        Self {
            client,
            retention: Arc::new(RwLock::new(retention)),
        }
    }

    pub fn retention_lock(&self) -> Arc<RwLock<Option<std::time::Duration>>> {
        self.retention.clone()
    }

    pub(super) async fn insert_batch<T>(
        &self,
        table: &'static str,
        entries: Vec<(Option<String>, Option<Vec<u8>>, T)>,
    ) -> OTelSdkResult
    where
        T: Serialize + Debug + Send + 'static,
    {
        // No retention means db export is off entirely — telemetry is opt-in
        // via a retention time (config `db_retention` or the runtime topic).
        // Persisting it without one would write immortal rows into a
        // replicated scope on every log line.
        let retention = {
            let read = self.retention.read().await;
            match *read {
                Some(retention) => retention,
                None => return Ok(()),
            }
        };
        let entry_count = entries.len();

        let result = self
            .client
            .write_tx_in_with_retention(scope(), Some(retention), async move |client, id| {
                let entries = entries
                    .into_iter()
                    .map(|(scope_name, eid, data)| {
                        let entry = ScopedEntry { scope_name, data };
                        let entry =
                            serde_json::to_vec(&entry).context("Failed to serialize entry")?;
                        anyhow::Ok((eid, entry))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;

                let req = tb_insert_batched::Request {
                    id,
                    op: tb_insert_batched::Op {
                        scope: scope(),
                        table: table.into(),
                        entries,
                    },
                };

                client
                    .send(req)
                    .await?
                    .map_err(|err| anyhow::anyhow!("{}", err.message))?;
                Ok(())
            })
            .await;

        crate::record_insert_batch_outcome(table, entry_count, Some(retention), result.as_ref());
        result.map_err(|err| OTelSdkError::InternalFailure(err.to_string()))
    }
}

pub use crate::export::ScopedEntry;
pub(crate) use crate::export::duration_since_unix_epoch;

pub fn scope() -> Scope {
    Scope {
        namespace: NAMESPACE_TELE.into(),
        database: DATABASE.into(),
        ..Default::default()
    }
}
