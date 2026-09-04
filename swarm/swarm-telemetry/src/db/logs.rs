use opentelemetry_proto::transform::{
    common::tonic::ResourceAttributesWithSchema, logs::tonic::group_logs_by_resource_and_scope,
};
use opentelemetry_sdk::{
    error::OTelSdkResult,
    logs::{LogBatch, LogExporter},
};

impl LogExporter for super::DbExporter {
    async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        let entries =
            group_logs_by_resource_and_scope(&batch, &ResourceAttributesWithSchema::default())
                .into_iter()
                .flat_map(|resource_logs| {
                    resource_logs.scope_logs.into_iter().flat_map(|scope_logs| {
                        let scope_name = scope_logs.scope.map(|scope| scope.name);
                        scope_logs
                            .log_records
                            .into_iter()
                            .map(move |log| (scope_name.clone(), None, log))
                    })
                })
                .collect();
        self.insert_batch(super::TABLE_LOGS, entries).await
    }
}
