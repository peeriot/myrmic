use opentelemetry_proto::transform::{
    common::tonic::ResourceAttributesWithSchema, trace::tonic::group_spans_by_resource_and_scope,
};
use opentelemetry_sdk::{
    error::OTelSdkResult,
    trace::{SpanData, SpanExporter},
};

impl SpanExporter for super::FileExporter {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let entries =
            group_spans_by_resource_and_scope(batch, &ResourceAttributesWithSchema::default())
                .into_iter()
                .flat_map(|resource_spans| {
                    resource_spans
                        .scope_spans
                        .into_iter()
                        .flat_map(|scoped_spans| {
                            let scope_name = scoped_spans.scope.map(|scope| scope.name);
                            scoped_spans
                                .spans
                                .into_iter()
                                .map(move |span| (scope_name.clone(), span))
                        })
                })
                .collect();

        self.append_lines(super::FILE_TRACES, entries)
    }
}
