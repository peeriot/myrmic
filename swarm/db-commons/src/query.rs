use zenoh::bytes::ZBytes;
use zenoh::query::Query;
use zenoh::sample::Sample;
use zenoh::time::Timestamp;

/// Source of the zenoh-level timestamp attached to successful replies.
///
/// Peers without a wall clock (the ESP modems) sync their hybrid logical
/// clocks from these reply timestamps.
pub trait ReplyTimestamp {
    /// The timestamp for an outgoing reply; `None` leaves it unstamped.
    fn reply_timestamp(&self) -> Option<Timestamp> {
        None
    }
}

#[allow(async_fn_in_trait)]
pub trait Handler<Req>: ReplyTimestamp + Sized
where
    Req: serde::de::DeserializeOwned,
{
    type Response: serde::Serialize;
    type Error: serde::Serialize;

    async fn call(self, req: Req, query: Query) {
        let timestamp = self.reply_timestamp();
        let result = self.handle(req).await;
        if let Err(err) = try_respond_to_query(&query, result, timestamp).await {
            tracing::error!("Unable to respond to query: {}", err);
        }
    }

    async fn handle(self, req: Req) -> Result<Self::Response, Option<Self::Error>>;
}

pub async fn respond_to_query<T, E>(
    query: &Query,
    result: Result<T, Option<E>>,
    timestamp: Option<Timestamp>,
) where
    T: serde::Serialize,
    E: serde::Serialize,
{
    try_respond_to_query(query, result, timestamp)
        .await
        .expect("unable to respond to query");
}

pub async fn try_respond_to_query<T, E>(
    query: &Query,
    result: Result<T, Option<E>>,
    timestamp: Option<Timestamp>,
) -> anyhow::Result<()>
where
    T: serde::Serialize,
    E: serde::Serialize,
{
    use anyhow::Context;

    match result {
        Ok(value) => {
            let bytes = postcard::to_allocvec(&value).context("Unable to serialise ok response")?;

            query
                .reply(query.key_expr(), bytes)
                .timestamp(timestamp)
                .await
                .map_err(|err| anyhow::anyhow!("Unable to send response through zenoh: {}", err))?;
        }
        Err(Some(value)) => {
            let bytes =
                postcard::to_allocvec(&value).context("Unable to serialise error response")?;

            query
                .reply_err(bytes)
                .await
                .map_err(|err| anyhow::anyhow!("Unable to send error through zenoh: {}", err))?;
        }
        Err(None) => (),
    }

    Ok(())
}

pub fn parse_bytes<T>(payload: &ZBytes) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    let bytes = payload.to_bytes();
    match postcard::from_bytes::<T>(&bytes) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!("Invalid payload received: {}", err);
            None
        }
    }
}

pub fn parse_sample<T>(sample: &Sample) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    tracing::debug!(
        "Attempting to parse incoming sample via {}",
        sample.key_expr()
    );

    let payload = sample.payload();
    parse_bytes(payload)
}

pub fn parse_query<T>(query: &Query) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    tracing::debug!(
        "Attempting to parse incoming query via {}",
        query.key_expr()
    );

    let bytes;
    let payload = match query.payload() {
        Some(bytes) => bytes,
        None => {
            bytes = Default::default();
            &bytes
        }
    };
    parse_bytes(payload)
}
