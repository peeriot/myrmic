//! Application-owned clock hooks.

use zenoh_protocol::core::Timestamp;
use zenoh_protocol::network::{NetworkBody, NetworkMessage, Push, Response};
use zenoh_protocol::zenoh::{Del, PushBody, Put, Reply, ResponseBody};

/// Hooks for an application-owned hybrid logical clock (e.g. a `uhlc::HLC`).
///
/// Register one with [`Session::set_clock`](crate::session::Session::set_clock):
/// the session runner then observes the timestamp of every received message,
/// and outgoing puts and replies are stamped with [`Self::timestamp`].
pub trait Clock: Sync {
    /// Called with the timestamp of every received message that carries one.
    fn observe(&self, timestamp: &Timestamp);

    /// The timestamp for an outgoing message; `None` sends it unstamped.
    fn timestamp(&self) -> Option<Timestamp>;
}

/// The wall-clock timestamp carried by `msg`, if any: the put/delete payload
/// of a push or a reply.
pub(crate) fn message_timestamp(msg: &NetworkMessage) -> Option<&Timestamp> {
    // `ReplyBody` is a type alias for `PushBody`, so one match covers both.
    let body = match &msg.body {
        NetworkBody::Push(Push { payload, .. }) => payload,
        NetworkBody::Response(Response {
            payload: ResponseBody::Reply(Reply { payload, .. }),
            ..
        }) => payload,
        _ => return None,
    };

    match body {
        PushBody::Put(Put { timestamp, .. }) | PushBody::Del(Del { timestamp, .. }) => {
            timestamp.as_ref()
        }
    }
}
