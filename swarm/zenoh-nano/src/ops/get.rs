//! Get operation

use core::future::IntoFuture;
use core::pin::Pin;
use core::time::Duration as StdDuration;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::DynamicReceiver;
use embassy_time::with_timeout;
use futures::Stream;
use futures::stream;
use zenoh_buffers::ZBuf;
use zenoh_buffers::buffer::Buffer;
use zenoh_protocol::core::{Reliability, WireExpr};
use zenoh_protocol::network::ext::QoSType as NQoSType;
use zenoh_protocol::network::request::ext::{NodeIdType, QueryTarget};
use zenoh_protocol::network::{NetworkBody, Request, RequestId};
use zenoh_protocol::zenoh::{ConsolidationMode, Query, RequestBody, ext::ValueType, query};

use crate::dispatch::{Dispatch, Route, Routed};
use crate::network::OutgoingMessage;
use crate::session::{Session, SessionError};

pub use zenoh_protocol::core::Encoding;

const DEFAULT_GET_TIMEOUT: StdDuration = StdDuration::from_secs(10);

/// Result of a completed [`Get`] operation.
pub enum GetResult {
    /// The peer replied successfully.
    Ok(ZBuf),
    /// The peer replied with an error.
    Err(ZBuf),
    /// Nothing arrived before the timeout elapsed. Distinct from an empty
    /// reply: nobody answered, rather than somebody answering with nothing.
    /// Callers that conflate the two turn an unreachable peer into a
    /// successful "not found".
    Timeout,
    /// The query completed — a `ResponseFinal` arrived — without any peer
    /// having replied. Also not an empty payload: there is nothing to decode.
    NoReply,
}

enum GetResponse {
    Reply(ZBuf),
    Err(ZBuf),
    Final,
}

/// A claimed dispatcher slot for an in-flight get. Frees the slot on drop.
struct GetSlot<'a> {
    dispatch: &'a dyn Dispatch,
    slot: usize,
    receiver: DynamicReceiver<'a, Routed>,
}

impl Drop for GetSlot<'_> {
    fn drop(&mut self) {
        self.dispatch.unregister(self.slot);
    }
}

/// Builder for a Zenoh get (query) operation.
///
/// Construct with [`Get::new`], optionally configure via builder methods, then:
/// - `.await` to receive the first reply, or
/// - `.stream().await` to receive all replies as a stream.
pub struct Get<'a, M: RawMutex, S: AsRef<str>> {
    session: Session<'a, M>,
    query: S,
    encoding: Encoding,
    payload: ZBuf,
    attachment: Option<ZBuf>,
    timeout: StdDuration,
}

impl<'a, M: RawMutex, S: AsRef<str>> Get<'a, M, S> {
    /// Create a new get builder for the given session and query expression.
    pub fn new(session: Session<'a, M>, query: S) -> Self {
        Self {
            session,
            query,
            encoding: Encoding::empty(),
            payload: ZBuf::empty(),
            attachment: None,
            timeout: DEFAULT_GET_TIMEOUT,
        }
    }

    /// Set the payload encoding.
    pub fn encoding(mut self, encoding: Encoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// Set the query payload.
    pub fn payload(mut self, payload: impl Into<ZBuf>) -> Self {
        self.payload = payload.into();
        self
    }

    /// Set the query attachment.
    pub fn attachment(mut self, attachment: impl Into<ZBuf>) -> Self {
        self.attachment = Some(attachment.into());
        self
    }

    /// Override the default timeout (10 seconds).
    pub fn timeout(mut self, timeout: StdDuration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Perform the query and return all replies as a stream.
    ///
    /// The stream ends when a matching `ResponseFinal` is received or when waiting for the next
    /// response exceeds the timeout.
    pub async fn stream(self) -> Result<impl Stream<Item = GetResult> + 'a, SessionError>
    where
        S: 'a,
    {
        let Self {
            session,
            query,
            encoding,
            payload,
            attachment,
            timeout,
        } = self;

        let (_request_id, slot) = send_get(
            session,
            query.as_ref(),
            encoding,
            payload,
            attachment,
            timeout,
        )
        .await?;

        let state = GetStreamState {
            slot,
            timeout,
            done: false,
        };

        Ok(stream::unfold(state, |mut state| async move {
            if state.done {
                return None;
            }
            let embassy_timeout = to_embassy_duration(state.timeout);
            match with_timeout(embassy_timeout, next_get_response(&state.slot)).await {
                Ok(GetResponse::Reply(payload)) => Some((GetResult::Ok(payload), state)),
                Ok(GetResponse::Err(payload)) => Some((GetResult::Err(payload), state)),
                Ok(GetResponse::Final) => None,
                // Yield the timeout before ending, so a caller can tell "nobody
                // answered in time" from "answered with nothing".
                Err(_) => {
                    state.done = true;
                    Some((GetResult::Timeout, state))
                }
            }
        }))
    }
}

impl<'a, M: RawMutex, S: AsRef<str> + 'a> IntoFuture for Get<'a, M, S> {
    type Output = Result<GetResult, SessionError>;
    type IntoFuture = Pin<Box<dyn core::future::Future<Output = Self::Output> + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let Self {
                session,
                query,
                encoding,
                payload,
                attachment,
                timeout,
            } = self;
            let query_str = query.as_ref();

            let (request_id, slot) =
                send_get(session, query_str, encoding, payload, attachment, timeout).await?;

            // Bound every wait locally. The dispatcher is lossy (a burst that overflows this get's
            // slot channel can drop responses, including the terminating `Final`), so relying on the
            // router-side `ext_timeout` alone could hang the caller forever if the `Final` is
            // dropped. Mirror the `stream()` path and time out locally instead.
            let embassy_timeout = to_embassy_duration(timeout);

            let result = match with_timeout(embassy_timeout, next_get_response(&slot)).await {
                Ok(GetResponse::Reply(reply_payload)) => {
                    trace!("Response for {} (rid={})", query_str, request_id);
                    GetResult::Ok(reply_payload)
                }
                Ok(GetResponse::Err(err_payload)) => {
                    trace!("Error response for {} (rid={})", query_str, request_id);
                    GetResult::Err(err_payload)
                }
                Ok(GetResponse::Final) => {
                    trace!(
                        "Response for {query_str} (rid={request_id}) got final response without a reply"
                    );
                    return Ok(GetResult::NoReply);
                }
                Err(_) => {
                    trace!("Query {query_str} (rid={request_id}) timed out with no response");
                    return Ok(GetResult::Timeout);
                }
            };

            // We already have the reply; wait for the `Final` terminator but don't block forever if
            // it was dropped - return what we have once the timeout elapses.
            loop {
                match with_timeout(embassy_timeout, next_get_response(&slot)).await {
                    Ok(GetResponse::Final) => {
                        trace!("Query {} got the response", request_id);
                        return Ok(result);
                    }
                    // A further reply (multi-reply query); the single-shot API keeps the first.
                    Ok(_) => {}
                    Err(_) => {
                        trace!("Query {request_id} timed out waiting for final; returning reply");
                        return Ok(result);
                    }
                }
            }
        })
    }
}

struct GetStreamState<'a> {
    slot: GetSlot<'a>,
    timeout: StdDuration,
    /// Set once a timeout has been surfaced, so the stream ends after it
    /// instead of yielding `Timeout` forever.
    done: bool,
}

async fn send_get<'a, M: RawMutex>(
    session: Session<'a, M>,
    query: &str,
    encoding: Encoding,
    payload: ZBuf,
    attachment: Option<ZBuf>,
    timeout: StdDuration,
) -> Result<(RequestId, GetSlot<'a>), SessionError> {
    let request_id = session.get_new_rid();

    // Grab the publisher before claiming a slot so an early failure doesn't leak a slot.
    let request_publisher = session.publisher()?;
    let dispatch = session.dispatch();
    let (slot, receiver) = session.register(Route::Get { request_id })?;

    let body = if payload.is_empty() {
        None
    } else {
        Some(ValueType { encoding, payload })
    };

    let wire_expression = WireExpr::empty().with_suffix(query);
    debug!("Querying: {}", query);
    request_publisher
        .publish(OutgoingMessage {
            body: NetworkBody::Request(Request {
                id: request_id,
                wire_expr: wire_expression.to_owned(),
                ext_qos: NQoSType::REQUEST,
                ext_tstamp: None,
                ext_nodeid: NodeIdType::default(),
                ext_target: QueryTarget::default(),
                ext_budget: None,
                ext_timeout: Some(timeout),
                payload: RequestBody::Query(Query {
                    consolidation: ConsolidationMode::Monotonic,
                    parameters: String::default(),
                    ext_sinfo: None,
                    ext_body: body,
                    ext_attachment: attachment.map(|buffer| query::ext::AttachmentType { buffer }),
                    ext_unknown: Vec::new(),
                }),
            }),
            reliability: Reliability::default(),
        })
        .await;

    Ok((
        request_id,
        GetSlot {
            dispatch,
            slot,
            receiver,
        },
    ))
}

/// Awaits the next response for a get. The dispatcher has already filtered by request id, so this
/// simply maps the routed payload; unexpected variants are ignored defensively.
async fn next_get_response(slot: &GetSlot<'_>) -> GetResponse {
    loop {
        match slot.receiver.receive().await {
            Routed::Reply(payload) => break GetResponse::Reply(payload),
            Routed::ReplyErr(payload) => break GetResponse::Err(payload),
            Routed::Final => break GetResponse::Final,
            // The dispatcher only ever routes responses to a get slot. Any other variant is an
            // internal routing bug: panic in debug/test builds, log and skip in release.
            _ => {
                debug_assert!(false, "get slot received a non-response routed message");
                error!("BUG: get slot received a non-response routed message; skipping");
            }
        }
    }
}

fn to_embassy_duration(duration: StdDuration) -> embassy_time::Duration {
    let micros = duration.as_micros().min(u64::MAX as u128) as u64;
    embassy_time::Duration::from_micros(micros)
}
