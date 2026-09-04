//! Subscribe Operation

use alloc::borrow::Cow;
use alloc::string::String;
use core::pin::pin;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::{DynamicReceiver, Sender};
use embassy_time::{Duration, Timer};
use zenoh_buffers::ZBuf;
use zenoh_protocol::core::{Reliability, WireExpr};
use zenoh_protocol::network::declare::SubscriberId;
use zenoh_protocol::network::declare::ext::NodeIdType;
use zenoh_protocol::network::ext::QoSType as NQoSType;
use zenoh_protocol::network::{Declare, DeclareBody, DeclareSubscriber, NetworkBody};

use crate::dispatch::{Dispatch, Route, Routed};
use crate::network::OutgoingMessage;
use crate::session::{Session, SessionError};

/// Zenoh Subscriber
///
/// Receives matching messages from its own dispatcher channel; the central dispatcher does the
/// keyexpr matching once and only delivers pushes that intersect this subscriber's key.
pub struct Subscriber<'a, S> {
    dispatch: &'a dyn Dispatch,
    slot: usize,
    receiver: DynamicReceiver<'a, Routed>,
    key: S,
}

impl<'a, S: AsRef<str>> Subscriber<'a, S> {
    /// Returns the subscriber's key
    pub fn key(&self) -> Cow<'_, str> {
        self.key.as_ref().into()
    }

    /// Declares the subscriber to the Network
    pub async fn declare<M: RawMutex>(
        session: Session<'a, M>,
        key: S,
    ) -> Result<Self, SessionError> {
        let sid = session.get_new_sid();

        debug!("Subscriber declare {}", key.as_ref());

        let msg = build_declare_msg(sid, key.as_ref());

        // Grab the publisher before claiming a slot so an early failure doesn't leak a slot.
        let publisher = session.publisher()?;
        let dispatch = session.dispatch();
        let (slot, receiver) = session.register(Route::Subscriber {
            key: String::from(key.as_ref()),
            redeclare: msg.clone(),
        })?;

        // Send the initial declare to the router. Re-declaration after a reconnect is handled
        // centrally by the dispatcher via the `redeclare` message stored above.
        publisher.publish(msg).await;

        Ok(Self {
            dispatch,
            slot,
            receiver,
            key,
        })
    }

    /// Receive a message from the subscriber.
    pub async fn receive(&mut self) -> Result<ZBuf, SessionError> {
        let (_key, payload) = self.receive_raw().await?;
        Ok(payload)
    }

    /// Like [`Self::receive`], but also returns the keyexpr the message was
    /// published on. With a wildcard subscription, this is how the caller
    /// learns which concrete key actually fired.
    pub async fn receive_keyed(&mut self) -> Result<(String, ZBuf), SessionError> {
        self.receive_raw().await
    }

    async fn receive_raw(&mut self) -> Result<(String, ZBuf), SessionError> {
        // The dispatcher only ever routes pushes to a subscriber slot. Any other variant is an
        // internal routing bug: panic in debug/test builds to catch it, and in release firmware
        // log an error and skip it rather than bricking the device.
        loop {
            match self.receiver.receive().await {
                Routed::Push { key, payload } => break Ok((key, payload)),
                _ => {
                    debug_assert!(false, "subscriber slot received a non-Push routed message");
                    error!("BUG: subscriber slot received a non-Push routed message; skipping");
                }
            }
        }
    }

    /// Continuously receive messages and send them into the provided channel
    ///
    /// # Arguments
    /// - `sender`: The channel sender to send received messages into
    /// - `await_on_full`: The duration to wait when the channel is full before dropping the message
    ///
    /// # Returns
    /// - `Ok(())`: The operation completed successfully
    /// - `Err(SessionError)`: An error occurred
    pub async fn run_receive_into<const N: usize>(
        &mut self,
        sender: Sender<'_, impl RawMutex, ZBuf, N>,
        await_on_full: Duration,
    ) -> Result<(), SessionError> {
        loop {
            let msg = self.receive().await?;

            let mut timeout = pin!(Timer::after(await_on_full));
            let mut send = pin!(sender.send(msg));

            if let Either::Second(()) = select(&mut send, &mut timeout).await {
                warn!("Dropping received message because the channel is full");
            }
        }
    }
}

impl<S> Drop for Subscriber<'_, S> {
    fn drop(&mut self) {
        self.dispatch.unregister(self.slot);
    }
}

/// Builds the `DeclareSubscriber` network message for `key`.
fn build_declare_msg(sid: SubscriberId, key: &str) -> OutgoingMessage {
    OutgoingMessage {
        body: NetworkBody::Declare(Declare {
            interest_id: None,
            ext_qos: NQoSType::DECLARE,
            ext_tstamp: None,
            ext_nodeid: NodeIdType::DEFAULT,
            body: DeclareBody::DeclareSubscriber(DeclareSubscriber {
                id: sid,
                wire_expr: WireExpr::empty().with_suffix(key).to_owned(),
            }),
        }),
        reliability: Reliability::Reliable,
    }
}
