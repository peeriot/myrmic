//! Host side of the gateway routing resource a cell declares for itself.
//!
//! The owner of a route is always the *calling* cell, taken from its verified
//! identity rather than the request, so a cell can neither mount on another's
//! behalf nor take over a mount someone else holds.

use myrmic_common::gateway::{
    AssetMount, Fallback, GATEWAY_ERR_INVALID_MOUNT, GATEWAY_ERR_MOUNT_TAKEN,
    GATEWAY_ERR_NOT_FOUND, GATEWAY_ERR_WRITE_FAILED, MountRequest, UnmountRequest,
};
use myrmic_common::types::error::SUCCESS;
use sorg_common::gateway_config::{self, AssetConfig, Fallback as RouteFallback, GatewayRoute};
use tracing::error;
use wasmtime::Caller;

use crate::wasm::{
    cell::state::CellState,
    host_functions::{
        db::{current_tx, transform_scope},
        decode, tri,
    },
};

pub(crate) async fn gateway_mount(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
) -> i32 {
    let request: MountRequest = tri!(decode(
        &mut caller,
        req_ptr,
        req_len,
        "gateway mount request"
    ));

    let Some(mount) = normalize_mount(&request.mount) else {
        error!("gateway mount: invalid mount path '{}'", request.mount);
        return GATEWAY_ERR_INVALID_MOUNT;
    };

    let owner = *caller.data().sri();
    let session = caller.data().session().clone();
    let tx_id = tri!(current_tx(&mut caller).await);
    let db = db_client::v1::Client::new(&session);

    // A mount belongs to whoever claimed it first; re-mounting your own is how
    // a redeploy refreshes its route.
    match gateway_config::get_gateway_route_in_tx(&db, tx_id, &mount).await {
        Ok(Some(existing)) if existing.owner != owner => {
            error!(
                "gateway mount: '{mount}' is already owned by cell '{}'",
                existing.owner
            );
            return GATEWAY_ERR_MOUNT_TAKEN;
        }
        Ok(_) => {}
        Err(err) => {
            error!("gateway mount: failed to look up '{mount}': {err}");
            return GATEWAY_ERR_WRITE_FAILED;
        }
    }

    let assets = request
        .assets
        .map(|assets| asset_config(&mut caller, &owner, assets))
        .transpose();

    let assets = tri!(assets);

    let route = GatewayRoute {
        owner,
        mount: mount.clone(),
        assets,
        api: request.api.as_deref().and_then(normalize_mount),
        ws: request.ws.as_deref().and_then(normalize_mount),
    };

    if route.assets.is_none() && route.api.is_none() && route.ws.is_none() {
        error!("gateway mount: no routes defined '{mount}'");
        return GATEWAY_ERR_INVALID_MOUNT;
    }

    if let Err(err) = gateway_config::put_gateway_route_in_tx(&db, tx_id, &route).await {
        error!("gateway mount: failed to register '{mount}': {err}");
        return GATEWAY_ERR_WRITE_FAILED;
    }

    SUCCESS
}

pub(crate) async fn gateway_unmount(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
) -> i32 {
    let request: UnmountRequest = tri!(decode(
        &mut caller,
        req_ptr,
        req_len,
        "gateway unmount request"
    ));

    let Some(mount) = normalize_mount(&request.mount) else {
        return GATEWAY_ERR_INVALID_MOUNT;
    };

    let owner = *caller.data().sri();
    let session = caller.data().session().clone();
    let tx_id = tri!(current_tx(&mut caller).await);
    let db = db_client::v1::Client::new(&session);

    match gateway_config::get_gateway_route_in_tx(&db, tx_id, &mount).await {
        Ok(Some(existing)) if existing.owner == owner => {}
        Ok(_) => return GATEWAY_ERR_NOT_FOUND,
        Err(err) => {
            error!("gateway unmount: failed to look up '{mount}': {err}");
            return GATEWAY_ERR_WRITE_FAILED;
        }
    }

    if let Err(err) = gateway_config::deregister_gateway_route_in_tx(&db, tx_id, &mount).await {
        error!("gateway unmount: failed to remove '{mount}': {err}");
        return GATEWAY_ERR_WRITE_FAILED;
    }

    SUCCESS
}

/// Resolves the guest's asset section into stored config. An unspecified scope
/// means the cell's own asset scope, which the host derives rather than trusts.
fn asset_config(
    caller: &mut Caller<'_, CellState>,
    owner: &cell_protocol::Sri,
    assets: AssetMount,
) -> Result<AssetConfig, i32> {
    let scope = match assets.scope {
        Some(scope) => transform_scope(caller, scope)?,
        None => gateway_config::cell_asset_scope(owner),
    };

    Ok(AssetConfig {
        scope,
        index: assets.index.and_then(|index| normalize_mount(&index)),
        fallback: match assets.fallback {
            Fallback::Spa => RouteFallback::Spa,
            Fallback::None => RouteFallback::None,
        },
    })
}

/// Normalizes a mount or sub-path to a single leading slash and no trailing
/// one, so `chat`, `/chat` and `/chat/` all name the same route. `None` if the
/// path is empty or has no content beyond slashes — except for the bare root,
/// which stays `/`.
fn normalize_mount(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().all(|c| c == '/') {
        return Some(String::from("/"));
    }

    Some(format!("/{}", trimmed.trim_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::normalize_mount;

    #[test]
    fn mount_normalization() {
        assert_eq!(normalize_mount("/chat").as_deref(), Some("/chat"));
        assert_eq!(normalize_mount("chat").as_deref(), Some("/chat"));
        assert_eq!(normalize_mount("/chat/").as_deref(), Some("/chat"));
        assert_eq!(normalize_mount("  /chat  ").as_deref(), Some("/chat"));
        assert_eq!(normalize_mount("/chat/api").as_deref(), Some("/chat/api"));

        // The catch-all survives normalization.
        assert_eq!(normalize_mount("/").as_deref(), Some("/"));
        assert_eq!(normalize_mount("///").as_deref(), Some("/"));

        // Nothing to mount at.
        assert_eq!(normalize_mount(""), None);
        assert_eq!(normalize_mount("   "), None);
    }
}
