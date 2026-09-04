use std::time::SystemTime;

use opentelemetry_proto::transform::{
    common::tonic::ResourceAttributesWithSchema, trace::tonic::group_spans_by_resource_and_scope,
};
use opentelemetry_sdk::{
    error::OTelSdkResult,
    trace::{SpanData, SpanExporter},
};

impl SpanExporter for super::DbExporter {
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
                            scoped_spans.spans.into_iter().map(move |span| {
                                // by default eid is a UUIDv7
                                // we want to add the span ID to the UUID but still keep it ordered
                                // by time as default. for that reason we retrieve the seconds since
                                // UNIX_EPOCH
                                let time =
                                    super::duration_since_unix_epoch(SystemTime::now()).as_secs();

                                // big endian bytes of the timestamp
                                let mut eid = time.to_be_bytes().to_vec();

                                // now adding the u64 span id which is encoded as Vec<u8> here
                                eid.extend(span.span_id.iter());

                                (scope_name.clone(), Some(eid), span)
                            })
                        })
                })
                .collect();
        self.insert_batch(super::TABLE_TRACES, entries).await
    }
}
