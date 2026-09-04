use std::{fmt::Display, time::Duration};

use cell_mailbox::{EventStream, Mailbox};
use cell_protocol::MailboxEvent;
use myrmic_common::cells::Event;
use sorg_common::{OutgoingMessage, custom_err};

use crate::{Client, Result};

// Best-effort backstop; promptness comes from the event subscription.
const EVENT_POLL_INTERVAL: Duration = Duration::from_secs(5);

impl Client {
    pub fn publish_cell_event(
        &self,
        event: &str,
        payload: Option<Vec<u8>>,
    ) -> impl Future<Output = Result<()>> {
        self.publish_cell_event_trace(event, payload, None)
    }

    pub async fn publish_cell_event_trace(
        &self,
        event: &str,
        payload: Option<Vec<u8>>,
        trace: Option<(u128, u64)>,
    ) -> Result<()> {
        let event: Event = event.try_into().map_err(|msg| custom_err!("{msg}"))?;
        let mut msg = OutgoingMessage::event(&event, payload)?;
        msg.attach_span_context(trace);
        msg.send(self.session(), None).await?;
        Ok(())
    }

    pub async fn subscribe_cell_event(
        &mut self,
        event: impl TryInto<Event, Error = impl Display>,
    ) -> Result<EventQueue> {
        let event: Event = event.try_into().map_err(|msg| custom_err!("{msg}"))?;
        let stream = Mailbox::new(self.session()).events(event).await?;
        Ok(EventQueue { stream })
    }
}

/// A cursored subscription to a cell event. Thin convenience wrapper over
/// [`cell_mailbox::EventStream`] that adds payload-only accessors and the
/// blocking/non-blocking method shapes callers rely on.
pub struct EventQueue {
    stream: EventStream,
}

impl EventQueue {
    /// Non-blocking: the next available event payload, if any.
    pub async fn try_receive(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.try_receive_batch(1).await?.pop())
    }

    /// Non-blocking: up to `batch_size` available event payloads.
    pub async fn try_receive_batch(&mut self, batch_size: usize) -> Result<Vec<Vec<u8>>> {
        let events = self.stream.poll(batch_size).await?;
        Ok(events.into_iter().map(|e| e.payload).collect())
    }

    /// Blocks until at least one event is available, then reads up to 64 and
    /// discards them.
    pub async fn drain(&mut self) -> Result<()> {
        self.receive_batch(64).await?;
        Ok(())
    }

    /// Blocks until one event is available and returns its payload.
    pub async fn receive(&mut self) -> Result<Vec<u8>> {
        self.receive_raw().await.map(|e| e.payload)
    }

    /// Blocks until at least one event is available, returning up to
    /// `batch_size` payloads.
    pub async fn receive_batch(&mut self, batch_size: usize) -> Result<Vec<Vec<u8>>> {
        let events = self
            .receive_raw_batch(EVENT_POLL_INTERVAL, batch_size)
            .await?;
        Ok(events.into_iter().map(|e| e.payload).collect())
    }

    /// Blocks until one event is available and returns it with its metadata.
    pub async fn receive_raw(&mut self) -> Result<MailboxEvent> {
        let mut events = self.receive_raw_batch(EVENT_POLL_INTERVAL, 1).await?;
        let event = events
            .pop()
            .expect("receive_raw_batch should only return when there is a message");
        Ok(event)
    }

    /// Blocks until at least one event is available, returning up to
    /// `batch_size` events with metadata; `poll_interval` is the backstop.
    pub async fn receive_raw_batch(
        &mut self,
        poll_interval: Duration,
        batch_size: usize,
    ) -> Result<Vec<MailboxEvent>> {
        Ok(self.stream.receive_batch(poll_interval, batch_size).await?)
    }
}
