mod commands;
mod events;
mod timers;

// Make sure the modules #[host_function] are visible to the setup in this file
#[expect(clippy::wildcard_imports, reason = "Needed to help proc macros")]
use commands::*;
#[expect(clippy::wildcard_imports, reason = "Needed to help proc macros")]
use events::*;
#[expect(clippy::wildcard_imports, reason = "Needed to help proc macros")]
use timers::*;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::pin::Pin;

use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::NativeSymbol;

use crate::{Error, host_function_decl};

/// Sets up the cell imports
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        // Commands
        host_function_decl!(send_command, c"(*~)i"), // (ptr + len) -> i32
        // Events
        host_function_decl!(publish_event, c"(*~)i"), // (ptr + len) -> i32
        // Timers
        host_function_decl!(create_timer, c"(*~)i"), // (ptr + len) -> i32 (timer_id or error)
        host_function_decl!(cancel_timer, c"(i)i"),  // (timer_id) -> i32
    ]);

    // safety: C FFI
    let success = unsafe {
        sys::wasm_runtime_register_natives(
            c"cell".as_ptr(),
            native_symbols.as_ptr().cast_mut(),
            native_symbols.len() as u32,
        )
    };

    if success {
        Ok(native_symbols)
    } else {
        Err(Error::Import)
    }
}
