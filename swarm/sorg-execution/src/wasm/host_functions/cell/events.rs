use crate::tri;
use crate::wasm::cell::state::CellState;
use crate::wasm::host_functions::decode;
use myrmic_common::cells::EventPublishRequest;
use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};
use tracing::error;
use wasmtime::Caller;

pub(crate) async fn publish_event(
    mut caller: Caller<'_, CellState>,
    buffer_ptr: u32,
    length: u32,
) -> i32 {
    let req = tri!(decode(
        &mut caller,
        buffer_ptr,
        length,
        "publish event request"
    ));

    let sri = *caller.data().sri();

    match publish_event_impl(&mut caller, req) {
        Ok(()) => SUCCESS,
        Err(err) => {
            error!("publish error cell {sri}: {err}");
            GENERIC_ERROR
        }
    }
}

pub(super) fn publish_event_impl(
    caller: &mut Caller<'_, CellState>,
    req: EventPublishRequest,
) -> crate::Result<()> {
    let mut msg = sorg_common::OutgoingMessage::event(&req.event, req.payload)?;
    caller.data().decorate_outgoing(&mut msg);

    // Delivery is a write nobody reads back, so it rides along with the rest of
    // the handler's work instead of costing a round trip here.
    msg.defer_into(caller.data_mut().application())?;

    Ok(())
}
