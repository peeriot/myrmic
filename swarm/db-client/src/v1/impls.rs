use crate::v1::Client;
use db_commons::models::*;
use futures::StreamExt;
use zenoh_result::zerror;

#[cfg(feature = "nano")]
use alloc::{string::String, vec::Vec};

type RequestOutput<T> =
    zenoh_result::ZResult<Result<<T as Request>::Response, <T as Request>::Error>>;

// fitemeirl
#[allow(async_fn_in_trait)]
pub trait Request {
    type Response: serde::de::DeserializeOwned;
    type Error: serde::de::DeserializeOwned;

    async fn send(self, client: &Client) -> RequestOutput<Self>;
}

/// Every tx-scoped operation travels as a single-op [`tx_apply`] application
/// against the transaction it names: one op, left open, its response narrowed
/// back to the operation's own type.
impl<T> Request for Tx<T>
where
    T: Operation,
    T::Response: serde::de::DeserializeOwned,
    T::Error: serde::de::DeserializeOwned,
{
    type Response = T::Response;
    type Error = T::Error;

    async fn send(self, client: &Client) -> RequestOutput<Self> {
        let Tx { id, op } = self;

        let application = tx_apply::Request {
            target: tx_apply::Target::Existing(id),
            ops: Vec::from([op.into()]),
            finish: tx_apply::Finish::KeepOpen,
        };

        Ok(match application.send(client).await? {
            Ok(response) => narrow::<T>(response.last),
            Err(err) => Err(T::Error::from(err)),
        })
    }
}

/// Narrows an application's tail response back to the operation's own response
/// type. Both failures here mean the server replied with something this op
/// never asks for, which only a protocol bug produces.
fn narrow<T: Operation>(last: Option<TxOpResponse>) -> Result<T::Response, T::Error> {
    let Some(last) = last else {
        return Err(T::Error::from(tx_apply::Error {
            message: String::from("application returned no response for ") + T::NAME,
            index: None,
        }));
    };

    T::Response::try_from(last).map_err(|_| {
        T::Error::from(tx_apply::Error {
            message: String::from("application returned the wrong response for ") + T::NAME,
            index: None,
        })
    })
}

macro_rules! impl_tx_direct {
    ($($req:ident => $var:ident),* $(,)?) => {
        const _: () = {
            $(
                impl Request for $req::Request {
                    type Response = $req::Response;
                    type Error = $req::Error;

                    async fn send(self, client: &Client) -> RequestOutput<Self> {
                        client.direct(self.id.2, &DbRequest::$var(self)).await
                    }
                }
            )*
        };
    };
}

// Placement-free calls against a transaction the caller already holds.
impl_tx_direct! {
    tx_commit => TxCommit,
    tx_rollback => TxRollback,
}

impl Request for ping::Request {
    type Response = ping::Response;
    type Error = ping::Error;

    async fn send(self, client: &Client) -> RequestOutput<Self> {
        let result = client.broadcast(&DbRequest::Ping(self)).await?;
        futures::pin_mut!(result);

        Ok(result
            .next()
            .await
            .ok_or_else(|| zerror!("unable to connect to network"))?)
    }
}

impl Request for db_info::Request {
    type Response = Vec<db_info::Response>;
    type Error = db_info::Error;

    async fn send(self, client: &Client) -> RequestOutput<Self> {
        let responses = client
            .broadcast::<DbRequest, db_info::Response, db_info::Error>(&DbRequest::Info(self))
            .await?
            .collect::<Vec<_>>()
            .await;

        // We don't care about errors, we're just trying to find out who's there...
        let responses = responses
            .into_iter()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();

        Ok(Ok(responses))
    }
}

// @TODO (peeriot/swarm#746) jezza - 13 Feb 2026: Deadlock dectection and prevention.

/// A begin is an application that applies nothing: place a transaction, keep it
/// open, hand back its id. It exists as its own request only so the call sites
/// that just want a transaction do not have to spell out an empty application.
impl Request for tx_begin::Request {
    type Response = tx_begin::Response;
    type Error = tx_begin::Error;

    async fn send(self, client: &Client) -> RequestOutput<Self> {
        let tx_begin::Request {
            constraint,
            retention_period,
            access,
        } = self;

        let application = tx_apply::Request {
            target: tx_apply::Target::New {
                constraint,
                access,
                retention_period,
            },
            ops: Vec::new(),
            finish: tx_apply::Finish::KeepOpen,
        };

        Ok(match application.send(client).await? {
            Ok(response) => match response.tx {
                Some(id) => Ok(tx_begin::Response { id }),
                None => Err(tx_begin::Error {
                    message: String::from("begin returned no transaction"),
                }),
            },
            Err(err) => Err(tx_begin::Error {
                message: err.message,
            }),
        })
    }
}

/// The one write-side placement path. A [`tx_apply::Target::New`] application
/// resolves a holder and retries refusals against a fresh locate — the
/// resolution is usually what went stale, most commonly a quiesced drain
/// answering the locate faster than the live replica, and the re-locate round
/// itself paces the retry. A [`tx_apply::Target::Existing`] one is a direct
/// call to the node already holding the transaction, so there is nothing to
/// resolve and nothing to retry: the ops are not idempotent, and a transport
/// error leaves the transaction's fate unknown, so it propagates and the caller
/// abandons it to the idle timeout.
impl Request for tx_apply::Request {
    type Response = tx_apply::Response;
    type Error = tx_apply::Error;

    async fn send(self, client: &Client) -> RequestOutput<Self> {
        const APPLY_ATTEMPTS: u32 = 3;

        let (constraint, access) = match &self.target {
            tx_apply::Target::Existing(id) => {
                let node = id.2;
                return client.direct(node, &DbRequest::TxApply(self)).await;
            }
            tx_apply::Target::New {
                constraint, access, ..
            } => (constraint.clone(), *access),
        };

        let req = DbRequest::TxApply(self);
        let mut refused = None;

        for _ in 0..APPLY_ATTEMPTS {
            let resolved = match &constraint {
                tx_begin::Constraint::Routed(scope) => {
                    locate_holder(client, scope, None, access).await?
                }
                tx_begin::Constraint::RoutedAt(scope, min_version) => {
                    locate_holder(client, scope, Some(*min_version), access).await?
                }
                tx_begin::Constraint::Ignore => any_node(client, None).await?,
            };

            let target_node = match resolved {
                Ok(node) => node,
                // A refused resolution (a version bound nobody meets yet, or
                // only drains visible) is retried like a refused application:
                // the next locate round often sees the announce that was
                // missing.
                Err(err) => {
                    refused = Some(tx_apply::Error {
                        message: err.message,
                        index: None,
                    });
                    continue;
                }
            };

            match client
                .direct::<DbRequest, tx_apply::Response, tx_apply::Error>(target_node, &req)
                .await?
            {
                Ok(response) => return Ok(Ok(response)),
                Err(err) => refused = Some(err),
            }
        }

        Ok(Err(refused.expect("at least one apply attempt ran")))
    }
}

impl Request for tb_peek::Request {
    type Response = tb_peek::Response;
    type Error = tb_peek::Error;

    async fn send(self, client: &Client) -> RequestOutput<Self> {
        // Routed like a read begin, and retried like one: a peek is idempotent,
        // and under load a locate round can drown and come back empty — the
        // old begin/list/rollback poll path absorbed that inside tx_begin's
        // own attempt loop, and a single-shot peek surfaced it straight into
        // hot poll loops (run 33245323105: the fan-in cell starved for whole
        // passes behind exactly these failures).
        //
        // Transport-level failures propagate immediately, exactly like
        // tx_begin's `?`: they mean *this session* cannot reach the mesh (most
        // commonly a runtime being torn down), and retrying them here burns
        // slow query timeouts and extra mesh-wide gathers precisely while the
        // next deployment is converging — a dying runtime's poll loops made
        // every third rack provision fail placement while this retried them
        // (runs 33246397138 / 33247056175 / 33247785686).
        const PEEK_ATTEMPTS: u32 = 3;

        let scope = self.scope.clone();
        let req = DbRequest::TbPeek(self);

        let mut failed: Option<tb_peek::Error> = None;

        for _ in 0..PEEK_ATTEMPTS {
            let resolved = match locate_holder(client, &scope, None, tx_begin::Access::Read).await?
            {
                Ok(node) => node,
                // A refused resolution: the next locate round often sees the
                // announce that was missing.
                Err(err) => {
                    failed = Some(tb_peek::Error {
                        message: err.message,
                    });
                    continue;
                }
            };

            match client
                .direct::<DbRequest, tb_peek::Response, tb_peek::Error>(resolved, &req)
                .await?
            {
                Ok(response) => return Ok(Ok(response)),
                Err(err) => failed = Some(err),
            }
        }

        Ok(Err(failed.expect("at least one peek attempt ran")))
    }
}

/// Chooses a node to route a scoped transaction to.
///
/// With discovery available (the `replica` feature on hosts, `nano` on
/// embedded), asks replicating nodes which of them hold `scope` at
/// `min_version` and picks the most caught-up. If none is known yet the scope
/// is being created, so any node will do — unless a `min_version` was
/// requested, which only a caught-up node could serve, so that is an error.
/// A candidate last seen within this window is treated as live. Mirrors the
/// server's peer TTL — its `peer_view` already drops anything older — so this
/// is a client-side backstop against relay latency and the self entry.
#[cfg(any(feature = "replica", feature = "nano"))]
const LOCATE_MAX_AGE_MS: u64 = 42_000;

async fn locate_holder(
    client: &Client,
    scope: &Scope,
    min_version: Option<Version>,
    access: tx_begin::Access,
) -> zenoh_result::ZResult<Result<NodeId, tx_begin::Error>> {
    #[cfg(any(feature = "replica", feature = "nano"))]
    {
        // Under load a locate round can drown and come back empty even though
        // a replica exists — and conceding to any_node then mints a brand-new
        // holder on whatever node the write lands on. Retrying turns that into
        // a slower write instead of a sicker mesh, and needs no timer: a
        // drowning round paces itself by waiting out the locate timeout, while
        // a genuinely new scope (no locate queryable declared anywhere)
        // finalises each round immediately and seeds fast. Embedded stays
        // single-round — WiFi locates are already slow, and its callers have
        // their own retries.
        #[cfg(feature = "replica")]
        const LOCATE_ROUNDS: u32 = 3;
        #[cfg(all(feature = "nano", not(feature = "replica")))]
        const LOCATE_ROUNDS: u32 = 1;

        #[cfg(feature = "replica")]
        let replica =
            crate::replica_v1::Client::new(&client.session, Subject::Scope(scope.clone()))?;

        let prefer_full = matches!(access, tx_begin::Access::Write);

        for _round in 0..LOCATE_ROUNDS {
            // One reply surfaces the whole live set, so a peer too slow to
            // answer the query itself is still vouched for here. The client,
            // not the responders, decides who is live enough and most
            // caught-up.
            //
            // Draining the whole reply set before selecting is not just about
            // completeness — it is what makes routing *deterministic*: every
            // locate (a registration's write, the placement read moments
            // later) runs the same comparison over the same full candidate set
            // and lands on the same node, which is the only read-your-writes
            // guarantee scopes without a configured replica set have. Exiting
            // early on the first selectable reply was tried (2026-08-28) and
            // reverted twice: reads early-exiting routed a placement read to a
            // replica that had not seen a seconds-old class registration
            // (`MissingArtifact(Wasm)` deploy failures on the rack), and
            // write-only early-exit failed the same way — the write landing on
            // the fastest responder while the read full-drained to the
            // highest-head claimant means the two no longer agree on the
            // holder. Any locate fast-path needs both sides to keep agreeing
            // (e.g. a shared scope→holder cache), not an asymmetric shortcut.
            #[cfg(feature = "replica")]
            let responses = replica.locate(scope, min_version).await?;

            #[cfg(all(feature = "nano", not(feature = "replica")))]
            let responses = locate_nano(client, scope, min_version).await?;

            let candidates = crate::v1::select::candidates(responses);

            let selected = crate::v1::select::select_holder(
                candidates,
                scope,
                min_version,
                LOCATE_MAX_AGE_MS,
                prefer_full,
            );

            if let Some(best) = selected {
                return Ok(Ok(best));
            }
        }

        if min_version.is_some() {
            return Ok(Err(tx_begin::Error {
                message: String::from("no replica holds the scope at the requested version"),
            }));
        }
    }

    #[cfg(not(any(feature = "replica", feature = "nano")))]
    let _ = (scope, min_version, access);

    // No known holder: fall through to any node. A `min_version` (only reachable
    // here without discovery) is reasserted node-side, which rejects if that
    // node is behind.
    //
    // Reaching here routinely means discovery is failing, not that the scope is
    // genuinely unheld — a locate that timed out looks exactly like one that
    // found nobody, and the fallback then broadcasts to every node.
    crate::log::warn!(
        "locate found no holder for {:?}; falling back to any node",
        scope
    );

    any_node(client, Some(scope)).await
}

/// A cap for embedded locates: WiFi round trips are slower than the host's
/// wired path, and the reply stream ends early at the final anyway.
#[cfg(all(feature = "nano", not(feature = "replica")))]
const LOCATE_TIMEOUT: core::time::Duration = core::time::Duration::from_millis(500);

/// Asks replicating nodes which of them hold `scope` at at least
/// `min_version`, returning each answering node's full locate response.
#[cfg(all(feature = "nano", not(feature = "replica")))]
async fn locate_nano(
    client: &Client,
    scope: &Scope,
    min_version: Option<Version>,
) -> zenoh_result::ZResult<Vec<locate::Response>> {
    use zenoh_nano::ops::get::{Get, GetResult};

    let ke =
        db_commons::topics::replica_query::format(&scope.namespace, &scope.database, &scope.schema);
    let data = postcard::to_allocvec(&locate::Request { min_version })
        .expect("unable to serialise locate request");

    let holders = Get::new(client.session, ke)
        .payload(data)
        .timeout(LOCATE_TIMEOUT)
        .stream()
        .await
        .map_err(|err| zerror!("unable to issue locate query: {:?}", err))?
        .filter_map(|item| async move {
            match item {
                GetResult::Ok(buf) => match crate::decode_zbuf::<locate::Response>(&buf) {
                    Ok(response) => Some(response),
                    Err(err) => {
                        crate::log::warn!("dropping malformed locate reply: {}", err);
                        None
                    }
                },
                GetResult::Err(buf) if crate::is_router_timeout(buf.to_zslice().as_slice()) => {
                    crate::log::warn!(
                        "locate: a queryable did not finalise within {}ms; its holders are \
                         missing from this locate",
                        LOCATE_TIMEOUT.as_millis()
                    );
                    None
                }
                GetResult::Err(_) => {
                    crate::log::warn!("locate: dropping an error reply from a peer");
                    None
                }
                GetResult::Timeout => {
                    crate::log::warn!(
                        "locate: timed out after {}ms with no response",
                        LOCATE_TIMEOUT.as_millis()
                    );
                    None
                }
                GetResult::NoReply => None,
            }
        })
        .collect::<Vec<_>>()
        .await;

    Ok(holders)
}

/// Picks any reachable node, deterministically.
///
/// With a scope in hand the pick is that scope's rendezvous winner over the
/// reachable set — the same `hash(scope, node)` argmax custody collapse draws
/// with (`cell-protocol`'s `custody_winner`) — so per-scope determinism holds
/// (a writer's fallback and a reader's fallback agree for that scope) while
/// different scopes land on different nodes. Without a scope
/// (`Constraint::Ignore`) there is nothing to hash, so the global max-id bias
/// remains.
///
/// A global bias is what this replaces, and it was measured on 2026-08-30:
/// **99.98% of every locate in the mesh** (6,382 of 6,383) resolved to the one
/// max-id node, because a fallback-minted sink answers locate as `Draining`
/// and a write locate filters drainers out — so every write fell back to the
/// same host forever. That is the ~450 ev/s ceiling.
///
/// This was tried once before and reverted (run 33260147891, zero events
/// delivered): a transaction routed by one scope may write *others*, so rows
/// land where the tx's anchor says while a reader of the written scope falls
/// back to that scope's own pick, and the two disagree. Nothing here fixes
/// that on its own — a stranded cross-scope write is still `Hidden` from
/// locate, so its reader finds no holder at all. What has changed is that the
/// prerequisite this revert named now exists: `tx_apply` ops each carry their
/// own target scope, so placement can follow the write rather than the anchor.
async fn any_node(
    client: &Client,
    scope: Option<&Scope>,
) -> zenoh_result::ZResult<Result<NodeId, tx_begin::Error>> {
    let ids: Vec<NodeId> = match client.send(db_info::Request {}).await? {
        Ok(value) => value.into_iter().map(|t| t.id).collect(),
        Err(err) => {
            return Ok(Err(tx_begin::Error {
                message: err.message,
            }));
        }
    };

    let picked = match scope {
        Some(scope) => ids
            .into_iter()
            .max_by_key(|id| (rendezvous_hash(scope, id), *id)),
        None => ids.into_iter().max(),
    };

    match picked {
        Some(id) => Ok(Ok(id)),
        None => Err(zerror!("no connected databases").into()),
    }
}
