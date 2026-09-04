use std::time::Duration;

use test_control_common::{
    Reply, Request, SorgPayload, TOPIC_CREATE_PUBLISHER, TOPIC_CREATE_QUERYABLE,
    TOPIC_CREATE_SUBSCRIBER, TOPIC_DELETE, TOPIC_DELETE_PUBLISHER, TOPIC_DELETE_QUERYABLE,
    TOPIC_DELETE_SUBSCRIBER, TOPIC_GET, TOPIC_HEALTH, TOPIC_INTROSPECTION, TOPIC_PUT, TOPIC_STATS,
    bail, is_query_timeout, zenoh_err,
};
use zenoh::Session;

use crate::Result;
use crate::config::Config;

/// Client for the interaction with the test control plugin.
#[derive(Debug, Clone)]
pub struct Client {
    pub(crate) config: Config,
    session: Session,
}

impl Client {
    #[must_use]
    pub fn new(session: Session) -> Self {
        Self {
            session,
            config: Config::default(),
        }
    }

    #[must_use]
    pub fn new_with_config(session: Session, config: Config) -> Self {
        Self { config, session }
    }

    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Create a publisher on the swarm node via the zenoh-test-control plugin.
    ///
    /// The publisher repeatedly sends `payload` on `key_expr`.
    /// - `count`: optional number of messages to send. `None` = unbounded.
    /// - `delay`: optional delay between messages.
    ///
    /// Expected reply: `Reply::PublisherCreated`.
    pub async fn create_publisher(
        &self,
        zid: String,
        key_expr: String,
        payload: String,
        count: Option<u32>,
        delay: Option<Duration>,
    ) -> Result<Reply> {
        let payload = Request::CreatePublisher {
            zid,
            key_expr,
            payload,
            count,
            delay,
        };

        self.send_request(
            TOPIC_CREATE_PUBLISHER,
            payload,
            "Test control client deserializing create publisher response",
            "No response received for the publisher creation request",
            "publisher create",
            "Publisher creation error",
            |r| !matches!(r, Reply::PublisherCreated { .. }),
        )
        .await
    }

    /// Delete a publisher from the swarm node by its `pub_id`.
    ///
    /// The plugin cancels the running task and cleans up its resources.
    ///
    /// Expected reply: `Reply::PublisherDeleted`.
    pub async fn delete_publisher(&self, zid: String, pub_id: String) -> Result<Reply> {
        let payload = Request::DeletePublisher { zid, pub_id };

        self.send_request(
            TOPIC_DELETE_PUBLISHER,
            payload,
            "Test control client deserializing delete publisher response",
            "No response received for the publisher deletion request",
            "publisher delete",
            "Publisher deletion error",
            |r| !matches!(r, Reply::PublisherDeleted { .. }),
        )
        .await
    }

    /// Create a subscriber on the swarm node.
    ///
    /// Subscribes to `key_expr` and optionally forwards received samples to `stream_key`.
    /// - `max_samples`: optional cap after which the subscriber auto-unsubscribes.
    /// - `stream_key`: optional key to forward each received payload to.
    ///
    /// Expected reply: `Reply::SubscriberCreated`.
    pub async fn create_subscriber(
        &self,
        zid: String,
        key_expr: String,
        max_samples: Option<u32>,
        stream_key: Option<String>,
    ) -> Result<Reply> {
        let payload = Request::CreateSubscriber {
            zid,
            key_expr,
            max_samples,
            stream_key,
        };

        self.send_request(
            TOPIC_CREATE_SUBSCRIBER,
            payload,
            "Test control client deserializing create subscriber response",
            "No response received for the subscriber creation request",
            "subscriber create",
            "Subscriber creation error",
            |r| !matches!(r, Reply::SubscriberCreated { .. }),
        )
        .await
    }

    /// Delete a subscriber from the swarm node by its `sub_id`.
    ///
    /// The plugin cancels the subscriber and releases resources.
    ///
    /// Expected reply: `Reply::SubscriberDeleted`.
    pub async fn delete_subscriber(&self, zid: String, sub_id: String) -> Result<Reply> {
        let payload = Request::DeleteSubscriber { zid, sub_id };

        self.send_request(
            TOPIC_DELETE_SUBSCRIBER,
            payload,
            "Test control client deserializing delete subscriber response",
            "No response received for the subscriber deletion request",
            "subscriber delete",
            "Subscriber deletion error",
            |r| !matches!(r, Reply::SubscriberDeleted { .. }),
        )
        .await
    }

    /// Create a queryable service on the swarm node.
    ///
    /// The queryable answers incoming `get` queries for `key_expr` with `static_payload`.
    ///
    /// Expected reply: `Reply::QueryableCreated`.
    pub async fn create_queryable(
        &self,
        zid: String,
        key_expr: String,
        static_payload: String,
    ) -> Result<Reply> {
        let payload = Request::CreateQueryable {
            zid,
            key_expr,
            static_payload,
        };

        self.send_request(
            TOPIC_CREATE_QUERYABLE,
            payload,
            "Test control client deserializing create queryable response",
            "No response received for the queryable creation request",
            "queryable create",
            "Queryable creation error",
            |r| !matches!(r, Reply::QueryableCreated { .. }),
        )
        .await
    }

    /// Delete a queryable by its `qbl_id`.
    ///
    /// The plugin stops serving queries and removes the handler.
    ///
    /// Expected reply: `Reply::QueryableDeleted`.
    pub async fn delete_queryable(&self, zid: String, qbl_id: String) -> Result<Reply> {
        let payload = Request::DeleteQueryable { zid, qbl_id };

        self.send_request(
            TOPIC_DELETE_QUERYABLE,
            payload,
            "Test control client deserializing queryable deletion response",
            "No response received for the queryable deletion request",
            "queryable delete",
            "Queryable deletion error",
            |r| !matches!(r, Reply::QueryableDeleted { .. }),
        )
        .await
    }

    /// Put a value on the swarm node under `key_expr`.
    ///
    /// This is a one-shot write; it does not create a long-lived task.
    ///
    /// Expected reply: `Reply::Put`.
    pub async fn put(&self, zid: String, key_expr: String, payload: String) -> Result<Reply> {
        let payload = Request::Put {
            zid,
            key_expr,
            payload,
        };

        self.send_request(
            TOPIC_PUT,
            payload,
            "Test control client deserializing put response",
            "No response received for the put request",
            "put",
            "put error",
            |r| !matches!(r, Reply::Put { .. }),
        )
        .await
    }

    /// Get values for `key_expr` from the swarm node.
    ///
    /// Dispatches a query on the plugin, results are streamed by the task
    /// the plugin spawns.
    /// - `timeout_ms`: optional query timeout.
    ///
    /// Expected reply: `Reply::Get`.
    pub async fn get(
        &self,
        zid: String,
        key_expr: String,
        timeout_ms: Option<u64>,
    ) -> Result<Reply> {
        let payload = Request::Get {
            zid,
            key_expr,
            timeout_ms,
        };

        self.send_request(
            TOPIC_GET,
            payload,
            "Test control client deserializing get response",
            "No response received for the get request",
            "get",
            "get error",
            |r| !matches!(r, Reply::Get { .. }),
        )
        .await
    }

    /// Delete the values associated with `key_expr` on the swarm node.
    ///
    /// This removes the stored payload, it does not affect live publishers/subscribers.
    ///
    /// Expected reply: `Reply::Delete`.
    pub async fn delete(&self, zid: String, key_expr: String) -> Result<Reply> {
        let payload = Request::Delete { zid, key_expr };

        self.send_request(
            TOPIC_DELETE,
            payload,
            "Test control client deserializing delete response",
            "No response received for the delete request",
            "delete",
            "delete error",
            |r| !matches!(r, Reply::Delete { .. }),
        )
        .await
    }

    /// Return statistics for `key_expr` collected by the plugin.
    ///
    /// The reply includes counters such as `sent`, `received`, `gets`, and `queries`
    /// as observed by the test-control plugin on the target node.
    ///
    /// Expected reply: `Reply::Stats`.
    pub async fn stats(&self, zid: String, key_expr: String) -> Result<Reply> {
        let payload = Request::Stats { zid, key_expr };

        self.send_request(
            TOPIC_STATS,
            payload,
            "Test control client deserializing stats response",
            "No response received for the stats request",
            "stats",
            "stats error",
            |r| !matches!(r, Reply::Stats { .. }),
        )
        .await
    }

    pub async fn health(&self, zid: String) -> Result<Reply> {
        let payload = Request::Health { zid };

        self.send_request(
            TOPIC_HEALTH,
            payload,
            "Test control client deserializing health response",
            "No response received for the health request",
            "health",
            "health error",
            |r| !matches!(r, Reply::Health { .. }),
        )
        .await
    }

    pub async fn introspection(&self, zid: String) -> Result<Reply> {
        let payload = Request::Introspection { zid };

        self.send_request(
            TOPIC_INTROSPECTION,
            payload,
            "Test control client deserializing introspection response",
            "No response received for the introspection request",
            "introspection",
            "introspection error",
            |r| !matches!(r, Reply::Introspection { .. }),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_request<F>(
        &self,
        topic: &str,
        payload: Request,
        deser_ctx: &'static str,
        no_response_msg: &'static str,
        request_prefix: &'static str,
        err_prefix: &'static str,
        is_expected: F,
    ) -> Result<Reply>
    where
        F: Fn(&Reply) -> bool,
    {
        let payload = payload.to_payload()?;
        let pending = self
            .session
            .get(topic)
            .timeout(self.config.query_timeout())
            .payload(payload)
            .await
            .map_err(|zen_err| zenoh_err!("get: topic {topic}", zen_err))?;

        let reply = match pending.recv_async().await {
            Ok(r) => r,
            Err(err) => bail!("{no_response_msg}: {err}"),
        };

        match reply.into_result() {
            Ok(sample) => {
                let reply = Reply::from_payload(sample.payload(), deser_ctx)?;
                if is_expected(&reply) {
                    bail!("Received wrong reply type for {request_prefix} request");
                }

                Ok(reply)
            }

            Err(repl_error) if is_query_timeout(&repl_error) => bail!(
                "Timeout of the {action} query of the test control client after the configured timeout of {timeout:?}",
                action = request_prefix,
                timeout = self.config.query_timeout()
            ),

            Err(err) => {
                let err_msg = err.payload().try_to_string()?;
                bail!("{err_prefix}: {err_msg}");
            }
        }
    }
}
