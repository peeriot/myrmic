use std::future::ready;

use opentelemetry_proto::transform::{
    common::tonic::ResourceAttributesWithSchema, logs::tonic::group_logs_by_resource_and_scope,
};
use opentelemetry_sdk::{
    error::OTelSdkResult,
    logs::{LogBatch, LogExporter},
};

impl LogExporter for super::FileExporter {
    fn export(&self, batch: LogBatch<'_>) -> impl Future<Output = OTelSdkResult> + Send {
        let entries =
            group_logs_by_resource_and_scope(&batch, &ResourceAttributesWithSchema::default())
                .into_iter()
                .flat_map(|resource_logs| {
                    resource_logs.scope_logs.into_iter().flat_map(|scope_logs| {
                        let scope_name = scope_logs.scope.map(|scope| scope.name);
                        scope_logs
                            .log_records
                            .into_iter()
                            .map(move |log| (scope_name.clone(), log))
                    })
                })
                .collect();

        ready(self.append_lines(super::FILE_LOGS, entries))
    }
}
