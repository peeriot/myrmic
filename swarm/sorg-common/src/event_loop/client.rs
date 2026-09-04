use tokio::sync::mpsc::{Sender, error::SendError};

pub(crate) type ClientSendError<T> = tokio::sync::mpsc::error::SendError<T>;

/// Used to send ``Event``s to the event loop. Offers a cheap way of cloning to create new
/// client handles
pub struct Client<Event> {
    sender: Sender<Event>,
}

impl<Event> Client<Event> {
    #[must_use]
    pub fn new(sender: Sender<Event>) -> Self {
        Self { sender }
    }

    #[must_use]
    pub fn handle(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }

    pub async fn send(&self, event: Event) -> Result<(), SendError<Event>> {
        self.sender.send(event).await?;
        Ok(())
    }
}

impl<Event> Clone for Client<Event> {
    fn clone(&self) -> Self {
        self.handle()
    }
}
