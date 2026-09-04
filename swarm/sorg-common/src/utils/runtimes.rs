use zenoh::{
    Session,
    query::{ConsolidationMode, QueryTarget},
};

use crate::{
    Error, ExecRuntimeInfo, OrchRuntimeRecord, Result, SorgPayload, TOPIC_EXEC_RUNTIMES,
    TOPIC_ORCH_RUNTIMES,
};

/// Queries the orch runtimes in the system and returns the records with their info
pub async fn query_orch_runtimes(session: &Session) -> Result<Vec<OrchRuntimeRecord>> {
    let replies = session
        .get(TOPIC_ORCH_RUNTIMES)
        .target(QueryTarget::All)
        // we want to hear all the answers
        .consolidation(ConsolidationMode::None)
        .await
        .map_err(|zen_err| Error::zenoh("querying available orch runtimes", zen_err))?;
    let mut orch_records: Vec<OrchRuntimeRecord> = vec![];
    while let Ok(reply) = replies.recv_async().await {
        let sample = reply.into_result().map_err(|_repl_err| {
            Error::custom("got error reply when querying orch runtimes in the system")
        })?;
        let orch_record: OrchRuntimeRecord =
            OrchRuntimeRecord::from_payload(sample.payload(), "deser orch rt query answer")?;
        orch_records.push(orch_record);
    }
    Ok(orch_records)
}

/// Queries the exec runtimes in the system and returns the records with their info
pub async fn query_exec_runtimes(session: &Session) -> Result<Vec<ExecRuntimeInfo>> {
    let replies = session
        .get(TOPIC_EXEC_RUNTIMES)
        .target(QueryTarget::All)
        // we want to hear all the answers
        .consolidation(ConsolidationMode::None)
        .await
        .map_err(|zen_err| Error::zenoh("querying available exec runtimes", zen_err))?;
    let mut exec_records: Vec<ExecRuntimeInfo> = vec![];
    while let Ok(reply) = replies.recv_async().await {
        let sample = reply.into_result().map_err(|_repl_err| {
            Error::custom("got error reply when querying exec runtimes in the system")
        })?;
        let orch_record: ExecRuntimeInfo =
            ExecRuntimeInfo::from_payload(sample.payload(), "deser exec rt query answer")?;
        exec_records.push(orch_record);
    }
    Ok(exec_records)
}
