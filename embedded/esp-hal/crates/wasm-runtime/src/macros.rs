//! Collection of macros that ease the implementation of host functions and handling of the runtime

pub(crate) use crate::host_function_decl;
pub(crate) use wasm_runtime_macros::*;

/// Unwraps a `Result`, returning the error out of the surrounding function on `Err`.
#[macro_export]
macro_rules! tri {
    ($expr:expr) => {{
        match $expr {
            Ok(value) => value,
            Err(err) => {
                return err;
            }
        }
    }};
}

/// Simplifies the declaration of host functions
///
/// # Example
///
/// The macro will take the name of the function import, the WAMR signature and will produce the
/// corresponding [`NativeSymbol`] that can be later registered with
/// `sys::wasm_runtime_register_natives(...)`. The expanded macro will also expect a
/// corresponding `$name_host_function` to be present to use as function pointer.
///
/// ```
/// let hf = host_function_decl!(test, c"(*~)i"); // (ptr + len) -> i32
///
/// #[unsafe(no_mangle)]
/// pub unsafe extern "C" fn test_host_function(
///     _exec_env: sys::wasm_exec_env_t,
///     buffer: *mut u8,
///     length: i32,
/// ) -> i32 {
///     // Do something
/// }
/// ```
#[macro_export]
macro_rules! host_function_decl {
    ($name:expr, $sig:literal) => {
        paste::paste! {
            wamr_rust_sdk::sys::NativeSymbol {
                // we want this to live as long as everything lives
                symbol: concat!(stringify!($name), "\0").as_ptr().cast::<core::ffi::c_char>(),
                func_ptr: [<$name _host_function>] as *mut core::ffi::c_void,
                signature: $sig.as_ptr(),
                attachment: core::ptr::null_mut(),
            }
        }
    };
}
