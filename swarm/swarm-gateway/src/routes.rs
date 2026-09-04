use crate::RoutingView;
use cell_protocol::{GATEWAY_CONFIG_TABLE, gateway_config_scope};
use db_client::Session;
use db_client::v1::models::Subject;
use myrmic_common::cells::{Sri, sri_of_path};
use sorg_common::gateway_config::{GatewayRoute, list_gateway_routes};
use sorg_common::placement_exists;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Routing table shared between the discovery task (writer) and the request
/// handlers (readers). Kept sorted by descending mount length for
/// longest-prefix matching.
pub type Routes = Arc<RwLock<Vec<GatewayRoute>>>;

/// How often to re-list the route registry as a backstop for missed
/// subscription pokes (which are best-effort).
const ROUTE_RECONCILE_INTERVAL: Duration = Duration::from_secs(15);

/// Finds the longest registered mount prefix that matches `path`.
pub fn match_route(routes: &Routes, path: &str) -> Option<GatewayRoute> {
    let guard = routes.read().ok()?;
    // Already sorted longest-first, so the first match is the most specific.
    guard
        .iter()
        .find(|r| mount_matches(&r.mount, path))
        .cloned()
}

/// Whether `path` falls under the mount prefix `mount` on a segment boundary.
fn mount_matches(mount: &str, path: &str) -> bool {
    if mount == "/" {
        return true;
    }
    path == mount || path.starts_with(&format!("{mount}/"))
}

/// Lists the registered routes and drops any whose owning cell is no longer in
/// the cell registry (self-heal). Fails open: on a per-route lookup error the
/// route is kept, so a transient hiccup never wipes the table.
///
/// This is what binds a route's lifetime to its cell. Undeploy also removes
/// routes eagerly; this covers the cases it cannot, such as a cell that went
/// away with the node hosting it.
async fn live_routes(
    session: &Session,
    routing: Option<&RoutingView>,
) -> anyhow::Result<Vec<GatewayRoute>> {
    let routes = list_gateway_routes(session)
        .await
        .map_err(|err| anyhow::anyhow!("unable to list gateway routes: {err}"))?;

    let mut live = Vec::with_capacity(routes.len());
    for route in routes {
        match placement_exists(session, &route.owner).await {
            Ok(false) => tracing::info!(
                "gateway: ignoring route for absent cell '{}' (mount {})",
                route.owner,
                route.mount
            ),
            // Present, or the lookup failed — keep it (fail open on errors).
            _ => {
                let Some(routing) = routing else {
                    live.push(route);
                    continue;
                };

                let routing = {
                    let Ok(guard) = routing.read() else {
                        tracing::warn!(
                            "gateway: ignoring route [routing config unavailable] '{}' (mount {})",
                            route.owner,
                            route.mount
                        );
                        continue;
                    };
                    guard.routes.clone()
                };

                let Some(routes) = routing else {
                    // No routes block was defined, so we accept it.
                    tracing::debug!(
                        "gateway: accepting route [no routes block] '{}' (mount {})",
                        route.owner,
                        route.mount
                    );
                    live.push(route);
                    continue;
                };

                let Some(config) = routes.get(&route.mount) else {
                    // A route config _wasn't_ found, so we ignore it.
                    tracing::info!(
                        "gateway: ignoring route [unable to find in config] '{}' (mount {})",
                        route.owner,
                        route.mount
                    );
                    continue;
                };

                // No defined owner, first come, first served.
                let Some(srn) = config.srn.as_deref() else {
                    tracing::debug!(
                        "gateway: accepting route (wildcard srn) '{}' (mount {})",
                        route.owner,
                        route.mount
                    );
                    live.push(route);
                    continue;
                };

                let sri = match sri_of_path(srn) {
                    Ok(sri) => Sri::from_uuid(sri),
                    Err(err) => {
                        tracing::info!(
                            "gateway: invalid configuration [srn '{}'] under {}: {}",
                            srn,
                            route.mount,
                            err
                        );
                        continue;
                    }
                };

                if sri == route.owner {
                    tracing::debug!(
                        "gateway: accepting route (matching owner) '{}' (mount {})",
                        route.owner,
                        route.mount
                    );
                    live.push(route);
                } else {
                    tracing::info!(
                        "gateway: ignoring route [incorrect owner ({})] '{}' (mount {})",
                        sri,
                        route.owner,
                        route.mount
                    );
                }
            }
        }
    }
    Ok(live)
}

/// Watches the `gateway-config` registry and keeps the routing table current.
///
/// Uses `db_client::subscribe` for prompt updates, with a periodic re-list as a
/// backstop for missed pokes (notifications are best-effort).
pub fn spawn_discovery(
    session: &Session,
    routing: Option<&RoutingView>,
) -> (Routes, tokio::task::JoinHandle<()>) {
    let routes: Routes = Arc::new(RwLock::new(Vec::new()));

    let handle = tokio::spawn({
        let session = session.clone();
        let routes = routes.clone();
        let routing = routing.cloned();

        async move {
            let client = db_client::v1::Client::new(&session);
            let notify = Arc::new(tokio::sync::Notify::new());

            let subscription = client
                .subscribe(
                    Subject::Scope(gateway_config_scope()),
                    GATEWAY_CONFIG_TABLE,
                    {
                        let notify = notify.clone();
                        move |_notification| notify.notify_one()
                    },
                )
                .await;

            let _subscription = match subscription {
                Ok(sub) => Some(sub),
                Err(err) => {
                    tracing::warn!(
                        "route change subscription failed; relying on periodic reconcile: {err}"
                    );
                    None
                }
            };

            let mut reconcile = tokio::time::interval(ROUTE_RECONCILE_INTERVAL);
            reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    () = notify.notified() => {}
                    _ = reconcile.tick() => {}
                }

                let mut new_routes = match live_routes(&session, routing.as_ref()).await {
                    Ok(new_routes) => new_routes,
                    Err(err) => {
                        tracing::warn!("route reconcile failed: {err}");
                        continue;
                    }
                };

                new_routes.sort_by_key(|route| std::cmp::Reverse(route.mount.len()));
                let count = new_routes.len();
                if let Ok(mut guard) = routes.write() {
                    *guard = new_routes;
                }
                tracing::info!("gateway routing table updated: {count} route(s)");
            }
        }
    });

    (routes, handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_matching() {
        assert!(mount_matches("/todos", "/todos"));
        assert!(mount_matches("/todos", "/todos/"));
        assert!(mount_matches("/todos", "/todos/assets/app.js"));
        assert!(!mount_matches("/todos", "/todosxyz"));
        assert!(!mount_matches("/todos", "/other"));
        // "/" is a catch-all.
        assert!(mount_matches("/", "/"));
        assert!(mount_matches("/", "/anything/here"));
    }
}
