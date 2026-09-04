use core::str;

use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};
use tracing::error;
use wasmtime::{AsContextMut, Caller};

use crate::wasm::cell::state::CellState;

use super::as_slice;

pub(super) fn report_error(mut caller: Caller<'_, CellState>, buffer_ptr: u32, length: u32) -> i32 {
    let data = as_slice(&mut caller, buffer_ptr as usize, length as usize);

    let Ok(msg) = str::from_utf8(data) else {
        error!("module logged using and invalid string");
        return GENERIC_ERROR;
    };
    let err_msg = msg.to_owned();
    caller.as_context_mut().data_mut().set_err_msg(err_msg);
    SUCCESS
}
