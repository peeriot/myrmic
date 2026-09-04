use core::ffi::c_int;

use cell_protocol::{CellAttachment, MailboxEvent};
use myrmic_common::cells::EventPublishRequest;
use myrmic_common::types::error::SUCCESS;
use myrmic_sdk::GENERIC_ERROR;
use wasm_runtime_macros::host_function;

use crate::async_request::cell_host::CellHost;
use crate::async_request::db::DbClient;
use crate::async_request::send_request_and_wait;

#[host_function]
fn publish_event(buffer: *mut u8, length: c_int) -> c_int {
    if buffer.is_null() {
        log::error!("buffer pointer is null");
        return GENERIC_ERROR;
    }

    // Deserialize event to publish
    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // safety: we already checked buffer is non-null
    let data = unsafe { core::slice::from_raw_parts(buffer, length as usize) };
    let Ok((pub_request, _rest)) = postcard::take_from_bytes::<EventPublishRequest>(data) else {
        log::error!("failed to deserialize cell event publish request");
        return GENERIC_ERROR;
    };

    publish_event_impl(pub_request)
}

/// Publishes an event via the Zenoh Client
pub(crate) fn publish_event_impl(ev: EventPublishRequest) -> c_int {
    let payload = ev.payload.unwrap_or_default();
    let mut attachment = CellAttachment::default();
    attachment.set_sender(Some(send_request_and_wait(CellHost::GetSri).as_uuid()));
    let event = MailboxEvent {
        event: ev.event,
        payload,
        attachment,
    };

    if send_request_and_wait(DbClient::PublishEvent { event }).is_err() {
        log::error!("failed to publish event");
        return GENERIC_ERROR;
    }

    SUCCESS
}
