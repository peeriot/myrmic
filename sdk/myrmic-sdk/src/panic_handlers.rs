/// Default heap size used when `WASM_SDK_HEAP_SIZE` is not set at compile time.
/// Or provided via the `[package.metadata.myrmic]` section in the user's Cargo.toml.
#[doc(hidden)]
pub const __DEFAULT_HEAP_SIZE: usize = 8 * 1024;

/// Wires up the global allocator, panic handler, and OOM handler to minimise the amount of
/// boilerplate required in cell crates.
///
/// The heap size is controlled by the build driver (myrmic), but can be overriden by a
/// `heap_size` in the `[package.metadata.myrmic]` table in your Cargo.toml.
///
/// ```toml
/// [package.metadata.myrmic]
/// heap_size = 2048
/// ```
#[macro_export]
macro_rules! cell_prelude {
    () => {
        const __WASM_SDK_HEAP_SIZE: usize = match option_env!("WASM_SDK_HEAP_SIZE") {
            Some(s) => $crate::__parse_usize(s),
            None => $crate::__DEFAULT_HEAP_SIZE,
        };
        $crate::define_alloc_heap!(__WASM_SDK_HEAP_SIZE);
        $crate::define_panic_handlers!();
    };
}

/// Compile-time `usize` parser (used by `cell_prelude!`, hence the assert message).
///
/// * Also supports underscores as digit separators (e.g. `16_384`).
#[doc(hidden)]
pub const fn __parse_usize(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut acc: usize = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'_' {
            i += 1;
            continue;
        }
        assert!(
            b >= b'0' && b <= b'9',
            "WASM_SDK_HEAP_SIZE must be a decimal integer"
        );
        acc = acc * 10 + (b - b'0') as usize;
        i += 1;
    }
    acc
}

/// Emits the cell's `#[panic_handler]`: it formats the panic message and
/// location into a pre-allocated buffer (no heap use), logs them at error
/// level, then traps to stop the module.
///
/// Invoked for you by [`cell_prelude!`](crate::cell_prelude); call it directly
/// only when opting out of the prelude.
#[macro_export]
macro_rules! define_panic_handlers {
    () => {
        const _: () = {
            use core::fmt::Write;

            // we want a pre-allocated buffer so that we don't need to allocate in case of a panic
            static BUF: $crate::__reexports::spin::Mutex<[u8; 192]> =
                $crate::__reexports::spin::Mutex::new([0; 192]);
            // message for re-entrant panics
            const BUSY_MSG: &str = "panic (buffer busy)";

            #[panic_handler]
            fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
                if let Some(mut guard) = BUF.try_lock() {
                    let ptr = guard.as_mut_ptr();
                    let cap = guard.len();

                    // Print reason
                    let mut w = $crate::RawBuf::new(ptr, cap);
                    let _ = w.write_str("wasm panic - ");
                    let _ = w.write_fmt(core::format_args!("{}", info.message()));
                    let _ = $crate::log_buffer(&w, $crate::LogLevel::Error);

                    // Print location if one is present
                    if let Some(location) = info.location() {
                        let mut w = $crate::RawBuf::new(ptr, cap);
                        let _ = w.write_str("          ↪ ");
                        let _ = w.write_fmt(core::format_args!("{location}"));
                        let _ = $crate::log_buffer(&w, $crate::LogLevel::Error);
                    }
                } else {
                    let _ = $crate::log(BUSY_MSG, $crate::LogLevel::Error);
                }

                // Cause a host trap to stop the module
                core::arch::wasm32::unreachable()
            }
        };
    };
}

/// The allocation-error handler lives in the SDK crate (rather than being emitted into each cell
/// crate by `cell_prelude!`) so that consuming modules do not need to enable the unstable
/// `#![feature(alloc_error_handler)]` themselves — the gate is carried by this crate alone.
#[cfg(all(feature = "alloc", target_arch = "wasm32"))]
const _: () = {
    use core::fmt::Write;

    // Allocation-free buffer, mirroring the panic handler's approach.
    static OOM_BUF: crate::__reexports::spin::Mutex<[u8; 192]> =
        crate::__reexports::spin::Mutex::new([0; 192]);

    #[alloc_error_handler]
    fn oom(layout: core::alloc::Layout) -> ! {
        if let Some(mut g) = OOM_BUF.try_lock() {
            let ptr = g.as_mut_ptr();
            let cap = g.len();
            let mut w = crate::RawBuf::new(ptr, cap);
            // tiny, allocation-free message
            let _ = w.write_fmt(core::format_args!(
                "wasm oom - size={} align={}; Double check your heap size.",
                layout.size(),
                layout.align()
            ));
            let _ = crate::log_buffer(&w, crate::LogLevel::Error);
        } else {
            let _ = crate::log("wasm oom (buffer busy)", crate::LogLevel::Error);
        }
        core::arch::wasm32::unreachable();
    }
};
