//! Functionality to test component behavior related to subscribing to/publishing on topics

use std::time::Duration;

use zenoh::{
    bytes::ZBytes,
    handlers::FifoChannelHandler,
    key_expr::OwnedKeyExpr,
    query::{QueryTarget, Reply},
};

use crate::{TestApp, WAIT_TIME};

impl TestApp {
    /// Sets up a queryable serving queries going to the provided topic
    /// does not reply to queries (we probably will need this later), just stores the query payload
    pub async fn set_up_queryable(&mut self, topic: OwnedKeyExpr) {
        let queryable = self
            .session()
            .declare_queryable(topic.clone())
            .await
            .unwrap();
        {
            let received_queries = self.received_queries.clone();
            tokio::spawn(async move {
                while let Ok(query) = queryable.recv_async().await {
                    let payload = match query.payload() {
                        Some(zbytes) => zbytes.to_bytes().to_vec(),
                        None => vec![],
                    };
                    received_queries
                        .lock()
                        .await
                        .entry(topic.clone())
                        .or_default()
                        .push(payload);
                }
            });
        }
        tokio::time::sleep(WAIT_TIME).await;
    }

    /// Sets up a queryable serving queries going to the provided topic
    /// stores the queries and replies to them using the provided function.
    /// The function describes how the query payload is transformed into the response payload
    pub async fn set_up_queryable_with_reply<F>(&mut self, topic: OwnedKeyExpr, resp_fn: F)
    where
        F: Fn(Vec<u8>) -> Result<Vec<u8>, Vec<u8>> + Send + Sync + 'static,
    {
        let queryable = self
            .session()
            .declare_queryable(topic.clone())
            .await
            .unwrap();
        {
            let received_queries = self.received_queries.clone();
            tokio::spawn(async move {
                while let Ok(query) = queryable.recv_async().await {
                    let payload = match query.payload() {
                        Some(zbytes) => zbytes.to_bytes().to_vec(),
                        None => vec![],
                    };
                    received_queries
                        .lock()
                        .await
                        .entry(topic.clone())
                        .or_default()
                        .push(payload.clone());
                    match resp_fn(payload) {
                        Ok(reply_success) => {
                            query.reply(query.key_expr(), reply_success).await.unwrap();
                        }
                        Err(reply_error) => query.reply_err(reply_error).await.unwrap(),
                    }
                }
            });
        }
        tokio::time::sleep(WAIT_TIME).await;
    }

    pub async fn received_query_payloads_on_topic(&mut self, topic: OwnedKeyExpr) -> Vec<Vec<u8>> {
        let mut guard = self.received_queries.lock().await;
        let payloads = guard.get_mut(&topic);
        let Some(payloads) = payloads else {
            return vec![];
        };
        std::mem::take(payloads)
    }

    pub async fn make_query(
        &self,
        topic: OwnedKeyExpr,
        payload: Option<ZBytes>,
    ) -> FifoChannelHandler<Reply> {
        let mut query = self
            .session()
            .get(topic)
            .timeout(Duration::from_secs(15)) // going with 15 seconds, since most default timeouts are 10
            .target(QueryTarget::All);
        if let Some(payload) = payload {
            query = query.payload(payload);
        }
        query.await.expect("failed runtime list query")
    }
}
