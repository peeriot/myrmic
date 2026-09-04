#![cfg_attr(feature = "nano", no_std)]

#[cfg(feature = "nano")]
extern crate alloc;

#[cfg(any(feature = "zenoh", feature = "nano"))]
pub mod application;
#[cfg(feature = "polled")]
mod polled;
#[cfg(feature = "replica")]
pub mod replica_v1;

#[cfg(feature = "polled")]
pub use polled::{PolledTable, Woke};

#[cfg(feature = "nano")]
use alloc::string::String;

pub use db_commons::NAMESPACE_SYS;

/// Per-level macros that route to `tracing` (zenoh feature) or `log` (nano feature).
#[allow(unused_macros, unused_imports)]
pub(crate) mod log {
    macro_rules! at {
        ($lvl:ident, $($args:tt)*) => {{
            #[cfg(feature = "zenoh")]
            ::tracing::$lvl!($($args)*);
            #[cfg(feature = "nano")]
            ::log::$lvl!($($args)*);
        }};
    }
    pub(crate) use at;

    macro_rules! error    { ($($a:tt)*) => { $crate::log::at!(error, $($a)*) } }
    macro_rules! warning  { ($($a:tt)*) => { $crate::log::at!(warn,  $($a)*) } }
    macro_rules! info     { ($($a:tt)*) => { $crate::log::at!(info,  $($a)*) } }
    macro_rules! debug    { ($($a:tt)*) => { $crate::log::at!(debug, $($a)*) } }
    pub(crate) use {debug, error, info, warning as warn};
}

pub mod v1;

#[cfg(feature = "nano")]
pub type Session = zenoh_nano::session::Session<'static>;

#[cfg(feature = "zenoh")]
pub type Session = zenoh::Session;

/// The payload zenoh's router synthesises when a queryable never sends its
/// `ResponseFinal` before the request's `ext_timeout` elapses (`QueryCleanup`).
const ROUTER_TIMEOUT_PAYLOAD: &[u8] = b"Timeout";

/// Whether an error reply is the router's timeout rather than an application
/// error. The error channel carries both, and the router replies with
/// `Encoding::default()` — exactly what `reply_err(bytes)` produces — so the
/// payload is the only discriminator available. A timeout means some queryable
/// matching the key expression never finalised, not that the peer rejected the
/// request; decoding it as the application's error type can never succeed.
fn is_router_timeout(payload: &[u8]) -> bool {
    payload == ROUTER_TIMEOUT_PAYLOAD
}

#[cfg(feature = "nano")]
fn decode_zbuf<T: serde::de::DeserializeOwned>(
    zbuf: &zenoh_nano::buffers::ZBuf,
) -> Result<T, postcard::Error> {
    let slice = zbuf.to_zslice();
    postcard::from_bytes(slice.as_slice())
}

/// Diagnostic summary of a reply payload that failed to decode: its length, and
/// its contents as text when valid UTF-8 (surfaces plain zenoh error strings and
/// empty bodies), otherwise a hex preview.
#[cfg(feature = "nano")]
fn describe_reply_bytes(bytes: &[u8]) -> String {
    use core::fmt::Write;
    const MAX: usize = 64;
    let mut out = String::new();
    let _ = write!(out, "{} bytes", bytes.len());
    match core::str::from_utf8(bytes) {
        Ok(text) => {
            let preview: String = text.chars().take(MAX).collect();
            let _ = write!(out, ", as text {preview:?}");
        }
        Err(_) => {
            let _ = write!(out, ", hex ");
            for b in bytes.iter().take(MAX) {
                let _ = write!(out, "{b:02x}");
            }
            if bytes.len() > MAX {
                let _ = write!(out, "…");
            }
        }
    }
    out
}

#[cfg(feature = "nano")]
pub(crate) async fn direct<R, T, E>(
    session: &Session,
    query: &str,
    req: &R,
) -> zenoh_result::ZResult<Result<T, E>>
where
    R: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
    E: serde::de::DeserializeOwned,
{
    use zenoh_nano::ops::get::GetResult;

    let data = postcard::to_allocvec(req).expect("unable to serialise request");

    match zenoh_nano::ops::get::Get::new(*session, query)
        .payload(data)
        .await
        .expect("boom")
    {
        GetResult::Ok(zbuf) => {
            let response: T = decode_zbuf(&zbuf)
                .map_err(|e| zenoh_result::zerror!("malformed reply from peer: {e}"))?;
            Ok(Ok(response))
        }
        GetResult::Err(zbuf) if is_router_timeout(zbuf.to_zslice().as_slice()) => {
            Err(zenoh_result::zerror!(
                "query '{query}' timed out: a matching queryable never finalised"
            )
            .into())
        }
        GetResult::Err(zbuf) => {
            let response: E = decode_zbuf(&zbuf)
                .map_err(|e| zenoh_result::zerror!("malformed error reply from peer: {e}"))?;
            Ok(Err(response))
        }
        GetResult::Timeout => {
            Err(zenoh_result::zerror!("query '{query}' timed out with no response").into())
        }
        GetResult::NoReply => {
            Err(zenoh_result::zerror!("query '{query}' completed with no reply").into())
        }
    }
}

#[cfg(feature = "nano")]
pub(crate) async fn broadcast<'a, R, T, E>(
    session: &'a Session,
    query: &str,
    req: &R,
) -> zenoh_result::ZResult<impl futures::Stream<Item = Result<T, E>> + 'a + use<'a, R, T, E>>
where
    R: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
    E: serde::de::DeserializeOwned,
{
    use futures::StreamExt;
    use zenoh_nano::ops::get::GetResult;

    let data = postcard::to_allocvec(req).expect("unable to serialise request");

    log::debug!("broadcasting query '{query}' ({} byte request)", data.len());

    let query = zenoh_nano::ops::get::Get::new(*session, String::from(query))
        .payload(data)
        .timeout(core::time::Duration::from_secs(5))
        .stream()
        .await
        .expect("boom")
        .filter_map(|item| async move {
            match item {
                GetResult::Ok(buf) => match decode_zbuf::<T>(&buf) {
                    Ok(response) => Some(Ok(response)),
                    Err(e) => {
                        let raw = buf.to_zslice();
                        log::warn!(
                            "dropping malformed reply from peer: {e} (expected {}, payload {})",
                            core::any::type_name::<T>(),
                            describe_reply_bytes(raw.as_slice()),
                        );
                        None
                    }
                },
                GetResult::Err(buf) if is_router_timeout(buf.to_zslice().as_slice()) => {
                    log::warn!(
                        "a queryable matching the request did not finalise in time; \
                         its reply is missing from this response set"
                    );
                    None
                }
                GetResult::Err(buf) => match decode_zbuf::<E>(&buf) {
                    Ok(response) => Some(Err(response)),
                    Err(e) => {
                        let raw = buf.to_zslice();
                        log::warn!(
                            "dropping malformed error reply from peer: {e} (expected error {}, payload {})",
                            core::any::type_name::<E>(),
                            describe_reply_bytes(raw.as_slice()),
                        );
                        None
                    }
                },
                GetResult::Timeout => {
                    log::warn!("request timed out with no response from any peer");
                    None
                }
                GetResult::NoReply => None,
            }
        });

    Ok(query)
}

#[cfg(feature = "zenoh")]
pub(crate) async fn direct<R, T, E>(
    session: &zenoh::Session,
    query: &str,
    req: &R,
) -> zenoh_result::ZResult<Result<T, E>>
where
    R: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
    E: serde::de::DeserializeOwned,
{
    use zenoh::query::ConsolidationMode;

    let data = postcard::to_allocvec(req).expect("unable to serialise request");

    let handler = session
        .get(query)
        .payload(data)
        .consolidation(ConsolidationMode::None)
        .await?;

    match handler.into_recv_async().await?.into_result() {
        Ok(reply) => {
            let reply = db_commons::query::parse_sample(&reply)
                .ok_or_else(|| zenoh_result::zerror!("malformed reply from peer"))?;
            Ok(Ok(reply))
        }
        Err(reply) => {
            if is_router_timeout(&reply.payload().to_bytes()) {
                return Err(zenoh_result::zerror!(
                    "query '{query}' timed out: a matching queryable never finalised"
                )
                .into());
            }
            let reply = db_commons::query::parse_bytes(reply.payload())
                .ok_or_else(|| zenoh_result::zerror!("malformed error reply from peer"))?;
            Ok(Err(reply))
        }
    }
}

#[cfg(feature = "zenoh")]
pub(crate) async fn broadcast<R, T, E>(
    session: &zenoh::Session,
    query: &str,
    req: &R,
) -> zenoh_result::ZResult<impl futures::Stream<Item = Result<T, E>> + use<R, T, E>>
where
    R: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
    E: serde::de::DeserializeOwned,
{
    use futures::StreamExt;
    use zenoh::query::{ConsolidationMode, QueryTarget};

    let data = postcard::to_allocvec(req).expect("unable to serialise request");

    let handler = session
        .get(query)
        .payload(data)
        .target(QueryTarget::All)
        .consolidation(ConsolidationMode::None)
        .await?
        .into_stream()
        .filter_map(|reply| async move {
            match reply.into_result() {
                Ok(reply) => match db_commons::query::parse_sample::<T>(&reply) {
                    Some(reply) => Some(Ok(reply)),
                    None => {
                        tracing::warn!("dropping malformed reply from peer");
                        None
                    }
                },
                Err(reply) if is_router_timeout(&reply.payload().to_bytes()) => {
                    tracing::warn!(
                        "a queryable matching the request did not finalise in time; \
                         its reply is missing from this response set"
                    );
                    None
                }
                Err(reply) => match db_commons::query::parse_bytes::<E>(reply.payload()) {
                    Some(reply) => Some(Err(reply)),
                    None => {
                        tracing::warn!("dropping malformed error reply from peer");
                        None
                    }
                },
            }
        });

    Ok(handler)
}
