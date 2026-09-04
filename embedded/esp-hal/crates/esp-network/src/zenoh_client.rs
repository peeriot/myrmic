use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use wasm_runtime::async_request::Error;
use wasm_runtime::async_request::zenoh::{
    RequestsReceiver, ResponsesSender, ZenohRequest as Request, ZenohResponse as Response,
};
use zenoh_nano::ops::publish::Publisher;
use zenoh_nano::session::Session;

/// Handles Zenoh requests and responses. `liveness` is invoked once per loop
/// iteration.
pub async fn client(
    session: Session<'static, NoopRawMutex>,
    requests: RequestsReceiver,
    responses: ResponsesSender,
    liveness: fn(),
) {
    log::trace!("[zenoh-client] Started");

    loop {
        // Liveness (observed): blocks on an empty request channel.
        liveness();

        match requests.receive().await {
            Request::Zid => {
                responses.send(Response::Zid(session.zid().await)).await;
            }
            Request::Get {
                topic,
                timeout,
                payload,
                attachment,
            } => {
                let mut req = zenoh_nano::ops::get::Get::new(session, topic);
                if let Some(t) = timeout {
                    req = req.timeout(t);
                }
                if let Some(p) = payload {
                    req = req.payload(p);
                }
                if let Some(a) = attachment {
                    req = req.attachment(a);
                }
                match req.await {
                    Ok(reply) => {
                        let zbuf = match reply {
                            zenoh_nano::ops::get::GetResult::Ok(z)
                            | zenoh_nano::ops::get::GetResult::Err(z) => Some(z),
                            zenoh_nano::ops::get::GetResult::Timeout => {
                                log::warn!("[zenoh-client] GET timed out with no response");
                                None
                            }
                            zenoh_nano::ops::get::GetResult::NoReply => {
                                log::warn!("[zenoh-client] GET completed with no reply");
                                None
                            }
                        };
                        match zbuf {
                            Some(z) => {
                                responses
                                    .send(Response::Get(Ok(z.to_zslice().to_vec())))
                                    .await;
                            }
                            None => responses.send(Response::Get(Err(Error::Generic))).await,
                        }
                    }
                    Err(e) => {
                        log::error!("[zenoh-client] GET request failed: {:?}", e);
                        responses.send(Response::Get(Err(Error::Generic))).await;
                    }
                }
            }
            Request::Put { topic, payload } => {
                let Ok(mut publisher) = Publisher::declare(session, topic).await else {
                    responses.send(Response::Put(Err(Error::Generic))).await;
                    continue;
                };
                responses
                    .send(Response::Put(
                        publisher
                            .publish(payload.into())
                            .await
                            .map_err(|_err| Error::Generic),
                    ))
                    .await;
            }
        }
    }
}
