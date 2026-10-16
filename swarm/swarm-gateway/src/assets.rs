//! Serving [`Asset`] replies out of the blob store.

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use cell_protocol::Sri;
use cell_protocol::guest_scope::resolve_guest_scope;
use db_client::v1::models::{Scope, path_resolve};
use myrmic_common::gateway::{Asset, Fallback};
use sorg_common::gateway_config::cell_asset_scope;
use zenoh::Session;

/// Serves `asset` on behalf of `cell`, the one that replied with it.
pub(crate) async fn serve(session: &Session, cell: Sri, asset: Asset) -> Response {
    let scope = match asset.scope {
        None => cell_asset_scope(&cell),
        Some(scope) => match resolve_guest_scope(&cell, scope) {
            Ok(scope) => scope,
            Err(code) => {
                tracing::warn!("cell {cell} asked to serve a scope it can't name ({code})");
                return (StatusCode::BAD_GATEWAY, "bad gateway\n").into_response();
            }
        },
    };

    let path = asset.path.as_str();
    let index = asset.index.as_deref();

    // The file to fetch: directories (and the root) resolve to the index.
    let primary = if path == "/" || path.is_empty() {
        index.map(with_leading_slash)
    } else if path.ends_with('/') {
        index.map(|i| format!("{path}{}", i.trim_start_matches('/')))
    } else {
        Some(path.to_owned())
    };

    let Some(primary) = primary else {
        return not_found();
    };

    match fetch_blob(session, &scope, &primary).await {
        Ok(Some(bytes)) => return asset_response(&primary, bytes),
        Ok(None) => {}
        Err(err) => return unavailable(&err),
    }

    // SPA fallback: a path that looks like a client-side route (no file
    // extension) falls back to the index document.
    if asset.fallback == Fallback::Spa
        && let Some(index) = index
        && !looks_like_file(&primary)
    {
        let index = with_leading_slash(index);
        match fetch_blob(session, &scope, &index).await {
            Ok(Some(bytes)) => return asset_response(&index, bytes),
            Ok(None) => {}
            Err(err) => return unavailable(&err),
        }
    }

    not_found()
}

/// Fetches a blob by path from the given scope, or `None` if it doesn't exist.
async fn fetch_blob(
    session: &Session,
    scope: &Scope,
    path: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let db = db_client::v1::Client::new(session);
    let scope = scope.clone();
    let path = with_leading_slash(path);

    let response = db
        .read_tx_in(scope.clone(), async move |client, tx_id| {
            client
                .send(path_resolve::Request {
                    id: tx_id,
                    op: path_resolve::Op {
                        scope,
                        path,
                        range: None,
                    },
                })
                .await
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to communicate with db: {err}"))?
        .map_err(|err| anyhow::anyhow!("path resolve failed: {}", err.message))?;

    Ok(response.blob.map(|blob| blob.blob))
}

/// Builds a 200 response for a served asset, inferring the MIME from its path.
fn asset_response(path: &str, bytes: Vec<u8>) -> Response {
    let content_type = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found\n").into_response()
}

fn unavailable(err: &anyhow::Error) -> Response {
    tracing::warn!("asset lookup failed: {err}");
    (StatusCode::SERVICE_UNAVAILABLE, "upstream unavailable\n").into_response()
}

/// Ensures a path has exactly one leading slash.
pub(crate) fn with_leading_slash(path: &str) -> String {
    format!("/{}", path.trim_start_matches('/'))
}

/// Heuristic: does the last path segment have a file extension?
fn looks_like_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_heuristic() {
        assert!(looks_like_file("/assets/app.js"));
        assert!(looks_like_file("/index.html"));
        assert!(!looks_like_file("/dashboard"));
        assert!(!looks_like_file("/users/42"));
        assert!(!looks_like_file("/"));
    }

    #[test]
    fn leading_slash() {
        assert_eq!(with_leading_slash("index.html"), "/index.html");
        assert_eq!(with_leading_slash("/index.html"), "/index.html");
        assert_eq!(with_leading_slash("///x"), "/x");
    }
}
