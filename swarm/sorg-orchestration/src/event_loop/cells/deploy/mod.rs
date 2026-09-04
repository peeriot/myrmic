mod cell_deploy;
mod deploy_cell;
pub(crate) mod placement;

use sorg_common::{CellFailureKind, DeploymentError, SorgPayload};
use tracing::warn;
use zenoh::query::Query;

use crate::error::Error;

/// Maps an orchestrator deploy error to the appropriate [`CellFailureKind`]:
/// a [`Error::DeploymentTimeout`] becomes [`CellFailureKind::Timeout`];
/// anything else becomes [`CellFailureKind::RuntimeReported`].
pub(super) fn cell_failure_kind(err: Error) -> CellFailureKind {
    match err {
        Error::DeploymentTimeout(_) => CellFailureKind::Timeout,
        other => CellFailureKind::RuntimeReported(other.to_string()),
    }
}

/// Replies to a deploy query with a postcard-serialized [`DeploymentError`].
pub(super) async fn reply_deployment_err(query: &Query, err: DeploymentError) {
    warn!("{err}");
    // DeploymentError is a plain serde enum — serialization cannot fail.
    let payload = err
        .to_payload()
        .expect("DeploymentError serialization is infallible");
    let _ = query.reply_err(payload).await;
}
