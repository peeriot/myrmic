//! The gateway (socket-routing) config registry.
//!
//! Cells register how the socket gateway should serve them here, through the
//! `gateway` host functions. Every `myrmic gateway` process discovers and
//! watches this registry (via `db_client::v1::Client::subscribe` on
//! [`cell_protocol::gateway_config_scope`]) and builds its routing table from
//! it. The registry lives in the network-replicated `sys` namespace so all
//! gateways see the same routes.
//!
//! One entry per mount, keyed (eid) by the mount path, recording the owning
//! cell. A route is the owner's resource: it is removed when the cell is
//! undeployed (see [`deregister_cell_routes`], called from orchestration's
//! undeploy path), and gateways drop routes whose owner has left the cell
//! registry.
//!
//! A [`GatewayRoute`] is modeled as typed, optional sections (assets / api /
//! oidc) rather than an ordered rule list; the gateway applies a fixed
//! precedence per request: OIDC guard → `WebSocket` upgrade → HTTP API → static
//! asset → fallback.

use cell_protocol::{GATEWAY_CONFIG_TABLE, Sri, gateway_config_scope};
use db_client::v1::{
    Client as DbClient,
    models::{Scope, TxId, blob_unlink, paths_list, tb_delete, tb_get, tb_insert, tb_list},
};
use serde::{Deserialize, Serialize};
use zenoh::Session;

use crate::{Result, custom_err};

/// How the gateway serves one cell, keyed by its mount prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayRoute {
    /// The cell that declared this route. Its lifecycle is bound to that cell:
    /// removed when the cell is undeployed, and dropped by gateways once the
    /// owner is no longer in the cell registry. Messages arriving on the
    /// route's API that name no target are addressed here.
    pub owner: Sri,

    /// URL path prefix this route is served under, e.g. `/chat` — the registry
    /// key (eid). The gateway does longest-prefix matching on the request path
    /// against every route.
    pub mount: String,

    /// Static front-end assets served under the mount. `None` for an
    /// API-only route.
    #[serde(default)]
    pub assets: Option<AssetConfig>,

    /// URL path (relative to `mount`) where the cell command/event API is
    /// served over HTTP. Clients send `CellInteraction`s here. `None` disables
    /// the HTTP API.
    #[serde(default)]
    pub api: Option<String>,

    /// URL path (relative to `mount`) for the `WebSocket` upgrade carrying
    /// `CellInteraction`s. `None` disables the `WebSocket` API.
    #[serde(default)]
    pub ws: Option<String>,
}

/// Static-asset serving config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetConfig {
    /// Blob-store scope the assets were uploaded into (and are served from).
    pub scope: Scope,
    /// File served at the mount root and (with [`Fallback::Spa`]) as the
    /// client-side-routing fallback, e.g. `/index.html`.
    #[serde(default)]
    pub index: Option<String>,
    /// What to serve when a path resolves to no asset.
    #[serde(default)]
    pub fallback: Fallback,
}

/// Behaviour when a requested asset path is not found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    /// Serve the index document (single-page-app client-side routing) for
    /// extensionless paths. The default.
    #[default]
    Spa,
    /// Return 404.
    None,
}

/// Inserts or replaces the route at a mount (keyed by [`GatewayRoute::mount`]),
/// inside the caller's transaction so the route lands with whatever else it
/// carries.
///
/// Callers are responsible for rejecting a mount already owned by a different
/// cell; this overwrites unconditionally.
pub async fn put_gateway_route_in_tx(
    db: &DbClient,
    tx_id: TxId,
    route: &GatewayRoute,
) -> Result<()> {
    let value = postcard::to_allocvec(route)
        .map_err(|_| custom_err!("failed to serialize gateway route"))?;

    db.send(tb_insert::Request {
        id: tx_id,
        op: tb_insert::Op {
            scope: gateway_config_scope(),
            table: GATEWAY_CONFIG_TABLE.to_owned(),
            eid: Some(route.mount.as_bytes().to_vec()),
            value,
        },
    })
    .await
    .map_err(|err| custom_err!("gateway route write failed: {err}"))?
    .map_err(|err| custom_err!("gateway route write failed: {}", err.message))?;

    Ok(())
}

/// Returns every registered gateway route.
pub async fn list_gateway_routes(session: &Session) -> Result<Vec<GatewayRoute>> {
    let db = DbClient::new(session);

    let response = db
        .read_tx_in(gateway_config_scope(), async move |client, tx_id| {
            client
                .send(tb_list::Request {
                    id: tx_id,
                    op: tb_list::Op {
                        scope: gateway_config_scope(),
                        table: GATEWAY_CONFIG_TABLE.to_owned(),
                        cursor: None,
                        limit: None,
                        order: None,
                    },
                })
                .await
        })
        .await
        .map_err(|err| custom_err!("unable to communicate with db: {err}"))?
        .map_err(|err| custom_err!("unable to list gateway routes: {}", err.message))?;

    response
        .entities
        .into_iter()
        .map(|(_id, value)| {
            postcard::from_bytes(&value)
                .map_err(|_| custom_err!("failed to deserialize gateway route"))
        })
        .collect()
}

/// Returns the route registered at `mount`, or `None` if there is none.
pub async fn get_gateway_route_in_tx(
    db: &DbClient,
    tx_id: TxId,
    mount: &str,
) -> Result<Option<GatewayRoute>> {
    let response = db
        .send(tb_get::Request {
            id: tx_id,
            op: tb_get::Op {
                scope: gateway_config_scope(),
                table: GATEWAY_CONFIG_TABLE.to_owned(),
                eid: mount.as_bytes().to_vec(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to communicate with db: {err}"))?
        .map_err(|err| custom_err!("unable to get gateway route: {}", err.message))?;

    match response.value {
        Some(bytes) => {
            let route = postcard::from_bytes(&bytes)
                .map_err(|_| custom_err!("failed to deserialize gateway route"))?;
            Ok(Some(route))
        }
        None => Ok(None),
    }
}

/// Removes the route at `mount`. Idempotent (deleting a missing route is not
/// an error).
pub async fn deregister_gateway_route_in_tx(db: &DbClient, tx_id: TxId, mount: &str) -> Result<()> {
    db.send(tb_delete::Request {
        id: tx_id,
        op: tb_delete::Op {
            scope: gateway_config_scope(),
            table: GATEWAY_CONFIG_TABLE.to_owned(),
            eid: mount.as_bytes().to_vec(),
        },
    })
    .await
    .map_err(|err| custom_err!("gateway route delete failed: {err}"))?
    .map_err(|err| custom_err!("gateway route delete failed: {}", err.message))?;

    Ok(())
}

/// Removes every route owned by `sri`.
///
/// Called from the cell-undeploy teardown path. Gateways also self-heal by
/// dropping routes whose owner has left the cell registry, so this is about
/// promptness rather than correctness; it reports the mounts it removed.
pub async fn deregister_cell_routes(session: &Session, sri: &Sri) -> Result<Vec<String>> {
    let owned: Vec<String> = list_gateway_routes(session)
        .await?
        .into_iter()
        .filter(|route| route.owner == *sri)
        .map(|route| route.mount)
        .collect();

    if owned.is_empty() {
        return Ok(owned);
    }

    let db = DbClient::new(session);
    let mounts = owned.clone();
    db.write_tx_in(gateway_config_scope(), async move |client, tx_id| {
        for mount in &mounts {
            if let Err(err) = deregister_gateway_route_in_tx(client, tx_id, mount).await {
                return Ok(Err(err));
            }
        }
        Ok(Ok(()))
    })
    .await
    .map_err(|err| custom_err!("gateway route delete failed: {err}"))??;

    Ok(owned)
}

/// The blob scope a cell serves its gateway assets from.
///
/// The same scope `myrmic_common::gateway::asset_scope` names to the guest:
/// the gateway namespace (replicated everywhere, so any gateway can read it),
/// isolated per cell by the schema component. The host resolves it from the
/// verified caller rather than trusting the guest, so a cell cannot claim
/// another's assets.
#[must_use]
pub fn cell_asset_scope(sri: &Sri) -> Scope {
    Scope::new(
        myrmic_common::gateway::NAMESPACE_GATEWAY,
        myrmic_common::gateway::ASSETS_DB,
        sri.to_string(),
    )
}

/// Unlinks every asset a cell uploaded, returning how many paths were removed.
///
/// The blobs themselves are content-addressed and shared, so only the paths go.
pub async fn purge_cell_assets(session: &Session, sri: &Sri) -> Result<usize> {
    let db = DbClient::new(session);
    let scope = cell_asset_scope(sri);

    let paths = {
        let scope = scope.clone();
        db.read_tx_in(scope.clone(), async move |client, tx_id| {
            client
                .send(paths_list::Request {
                    id: tx_id,
                    op: paths_list::Op { scope, limit: None },
                })
                .await
        })
        .await
        .map_err(|err| custom_err!("unable to communicate with db: {err}"))?
        .map_err(|err| custom_err!("unable to list cell assets: {}", err.message))?
        .paths
    };

    let count = paths.len();
    if count == 0 {
        return Ok(0);
    }

    db.write_tx_in(scope.clone(), async move |client, tx_id| {
        Ok(unlink_paths_in_tx(client, tx_id, scope, paths).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {err}"))??;

    Ok(count)
}

async fn unlink_paths_in_tx(
    client: &DbClient,
    tx_id: TxId,
    scope: Scope,
    paths: Vec<String>,
) -> Result<()> {
    for path in paths {
        client
            .send(blob_unlink::Request {
                id: tx_id,
                op: blob_unlink::Op {
                    scope: scope.clone(),
                    path,
                },
            })
            .await
            .map_err(|err| custom_err!("unable to send unlink request: {err}"))?
            .map_err(|err| custom_err!("unable to unlink cell asset: {}", err.message))?;
    }

    Ok(())
}
