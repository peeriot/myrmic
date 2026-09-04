//! Zenoh Async requests and responses

use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use zenoh_protocol::core::ZenohIdProto;

use wasm_runtime_macros::requests;

use crate::async_request::{Error, Request, Response, ResponseResult};

#[derive(Debug)]
pub struct ZenohContext {
    /// Sender for Zenoh requests
    pub requests: RequestsSender,
    /// Receiver for Zenoh responses
    pub responses: ResponsesReceiver,
}

const ZENOH_CLIENT_CAPACITY: usize = 2;
pub type Requests = Channel<CriticalSectionRawMutex, ZenohRequest, ZENOH_CLIENT_CAPACITY>;
pub type RequestsReceiver =
    Receiver<'static, CriticalSectionRawMutex, ZenohRequest, ZENOH_CLIENT_CAPACITY>;
pub type RequestsSender =
    Sender<'static, CriticalSectionRawMutex, ZenohRequest, ZENOH_CLIENT_CAPACITY>;
pub type Responses = Channel<CriticalSectionRawMutex, ZenohResponse, ZENOH_CLIENT_CAPACITY>;
pub type ResponsesReceiver =
    Receiver<'static, CriticalSectionRawMutex, ZenohResponse, ZENOH_CLIENT_CAPACITY>;
pub type ResponsesSender =
    Sender<'static, CriticalSectionRawMutex, ZenohResponse, ZENOH_CLIENT_CAPACITY>;

requests! {
    wrap(ZenohRequest => Request::Zenoh),
    unwrap(Response::Zenoh => ZenohResponse);

    Zid => ZenohIdProto,
    Get {
        topic: String,
        timeout: Option<Duration>,
        payload: Option<Vec<u8>>,
        attachment: Option<Vec<u8>>
    } => Result<Vec<u8>, Error>,
    Put { topic: String, payload: Vec<u8> } => ResponseResult,
}

pub async fn execute_request(
    ctx: &mut crate::async_request::Context,
    req: ZenohRequest,
) -> ZenohResponse {
    log::trace!("[async req][Zenoh] Received Request {req:?}");

    ctx.zenoh.requests.send(req).await;

    // Wait for response
    ctx.zenoh.responses.receive().await
}
