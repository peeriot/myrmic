use std::fmt::Debug;

use tokio::task::JoinHandle;
use tracing::debug;
use zenoh::{Session, query::Query};

use crate::{PoisonSnd, Result, poison_channel, zenoh_err};

use super::client::Client;

pub fn set_up_queryable<TEvent, TQuerable>(
    session: Session,
    client: Client<TEvent>,
    queryable: TQuerable,
) -> (JoinHandle<Result<()>>, PoisonSnd)
where
    TQuerable: QueryableTrait<EventLoopEvent = TEvent> + Send + 'static,
    TEvent: Send + Debug + 'static,
{
    let (poison_snd, mut poison_rcv) = poison_channel();
    let join_handle = tokio::spawn(async move {
        let topic = queryable.topic(&session);
        let name = queryable.name();
        let zenoh_queryable = session
            .declare_queryable(&topic)
            .await
            .map_err(|zen_err| zenoh_err!("declaring queryable on topic {topic}", zen_err))?;
        debug!("declared '{name}' queryable on topic '{topic}'");
        loop {
            tokio::select! {
                // Regular query
                query_result = zenoh_queryable.recv_async() => {
                    debug!("received '{name}' query");
                    let query_result = query_result.map_err(|zen_err| zenoh_err!("unpacking result of query on topic {topic}", zen_err))?;
                    client.send(queryable.event_from_query(query_result)).await?;
                }

                // Shutdown signal
                _ = &mut poison_rcv => {
                    break;
                }
            }
        }
        debug!("'{name}' queryable terminated");
        Ok(())
    });
    (join_handle, poison_snd)
}

pub trait QueryableTrait: Sized + Send + Sync {
    type EventLoopEvent: 'static + Debug + Send;

    fn topic(&self, session: &Session) -> String;

    fn name(&self) -> &'static str;

    fn event_from_query(&self, query: Query) -> Self::EventLoopEvent;
}
