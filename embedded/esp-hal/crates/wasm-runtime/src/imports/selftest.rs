//! Watchdog self-test host function (`wdt-selftest` feature, never shipped).
//!
//! Lets a HIL test deliberately trigger a liveness stall so the on-die
//! watchdog can be exercised end-to-end (SDS-FEAT-2026-HWD-001). A guest cell
//! calls [`wdt_selftest_wedge`] with a mode; the firmware's prio-1 stats task
//! polls [`wedge_mode`] and acts on it. The host function only records the
//! request — it runs on the WAMR (prio-0) thread, which cannot starve the
//! prio-1 executor the watchdog protects, so the actual wedge must happen on
//! the executor side.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::pin::Pin;
use core::sync::atomic::{AtomicU8, Ordering};

use myrmic_sdk::{GENERIC_ERROR, SUCCESS};
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::NativeSymbol;

use crate::Error;
use crate::macros::{host_function, host_function_decl};

/// Requested wedge mode: `0` none, `1` spin (wedge the whole executor), `2`
/// stall (a required task stops making progress while the executor stays
/// alive). Written by the guest via [`wdt_selftest_wedge`], polled by the
/// firmware.
static WEDGE_MODE: AtomicU8 = AtomicU8::new(0);

/// The currently-requested wedge mode (`0` when none).
pub(crate) fn wedge_mode() -> u8 {
    WEDGE_MODE.load(Ordering::Relaxed)
}

/// Registers the self-test import under the `selftest` WAMR module.
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        host_function_decl!(wdt_selftest_wedge, c"(i)i"), // (mode: i32) -> i32
    ]);

    // safety: C FFI
    let success = unsafe {
        sys::wasm_runtime_register_natives(
            c"selftest".as_ptr(),
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

/// Guest-callable trigger: record a deliberate liveness-wedge request. Returns
/// immediately; the firmware's prio-1 task performs the actual wedge.
#[host_function]
fn wdt_selftest_wedge(mode: i32) -> i32 {
    // Guest-callable: only the known modes (1 = spin, 2 = stall) are accepted.
    // Anything else is ignored so a bad value can't wedge a required task or
    // leave a persistent unknown mode latched.
    let stored: u8 = match mode {
        1 => 1,
        2 => 2,
        other => {
            log::warn!("[wdt-selftest] ignoring unknown wedge mode {other}");
            return GENERIC_ERROR;
        }
    };
    log::warn!("[wdt-selftest] wedge requested (mode {stored})");
    WEDGE_MODE.store(stored, Ordering::Relaxed);
    SUCCESS
}
