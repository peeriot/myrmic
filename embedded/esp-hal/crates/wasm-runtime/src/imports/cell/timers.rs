use core::ffi::c_int;

use myrmic_common::cells::{Command, CreateTimerRequest};
use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};
use postcard::take_from_bytes;
use wasm_runtime_macros::host_function;

use crate::async_request::cell_host::CellHost;
use crate::async_request::send_request_and_wait;

#[host_function]
fn create_timer(buffer: *mut u8, length: c_int) -> c_int {
    if buffer.is_null() {
        log::error!("buffer pointer is null");
        return GENERIC_ERROR;
    }
    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // safety: we already checked that buffer is non-null
    let data = unsafe { core::slice::from_raw_parts(buffer, length as usize) };
    let Ok((request, _)) = take_from_bytes::<CreateTimerRequest>(data) else {
        log::error!("failed to deserialize create timer request");
        return GENERIC_ERROR;
    };
    // Validate export name before we send request
    let Ok(command) = Command::new(request.export_name.clone()) else {
        log::error!(
            "{name} is not a valid command name",
            name = request.export_name
        );

        return GENERIC_ERROR;
    };
    if !send_request_and_wait(CellHost::CommandExists(command)) {
        log::error!("the requested timer doesn't exist on the cell");

        return GENERIC_ERROR;
    }

    // Create timer
    if let Ok(id) = send_request_and_wait(CellHost::CreateTimer(request)) {
        id.try_into().unwrap_or_else(|_| {
            log::error!("Failed to fit in i32");
            GENERIC_ERROR
        })
    } else {
        log::error!("failed to create timer");
        GENERIC_ERROR
    }
}

#[host_function]
fn cancel_timer(id: c_int) -> c_int {
    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    match send_request_and_wait(CellHost::CancelTimer(id as u32)) {
        Ok(()) => SUCCESS,
        Err(_) => {
            log::error!("failed to cancel timer: id={id}");
            GENERIC_ERROR
        }
    }
}
