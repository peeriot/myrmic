//! Import-link verification.
//!
//! WAMR links an unresolved host import as a deferred stub: the module
//! instantiates successfully and only traps when the guest actually calls the
//! missing function, with a cryptic `failed to call unlinked import function`.
//! That turns a wrong firmware profile - a cell importing
//! `selftest::wdt_selftest_wedge` on an image built without the `wdt-selftest`
//! feature, say - into a silent failure that surfaces far from its cause.
//!
//! Rejecting such a module before instantiation makes the deploy fail loudly
//! and early, with a message naming the missing import.

use alloc::string::{String, ToString};
use core::ffi::{CStr, c_char};
use core::mem::MaybeUninit;

use wamr_rust_sdk::module::Module;
use wamr_rust_sdk::sys;

use crate::Error;

/// Rejects a module that imports a host function the firmware does not provide.
///
/// Returns [`Error::UnlinkedImport`] naming the first unresolved function
/// import. Only function imports are checked: table, global and memory imports
/// do not carry the deferred-stub trap this guards against.
pub(crate) fn ensure_imports_linked(module: &Module<'_>) -> Result<(), Error> {
    // safety: C FFI; the module handle is valid for the borrow.
    let count = unsafe { sys::wasm_runtime_get_import_count(module.get_inner_module()) };

    for index in 0..count {
        let mut import = MaybeUninit::<sys::wasm_import_t>::uninit();
        // safety: C FFI populates the struct for a valid index.
        unsafe {
            sys::wasm_runtime_get_import_type(
                module.get_inner_module(),
                index,
                import.as_mut_ptr(),
            );
        }
        // safety: the C FFI function has populated the memory.
        let import = unsafe { import.assume_init() };

        if import.kind as core::ffi::c_uint
            != sys::wasm_import_export_kind_t_WASM_IMPORT_EXPORT_KIND_FUNC
        {
            continue;
        }
        if import.linked {
            continue;
        }

        return Err(Error::UnlinkedImport {
            module: cstr_to_string(import.module_name),
            name: cstr_to_string(import.name),
        });
    }

    Ok(())
}

/// A WAMR-owned C string into an owned Rust `String`, tolerating a null pointer
/// so the error path never dereferences one.
fn cstr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::from("<unknown>");
    }
    // safety: WAMR gives a valid NUL-terminated C string for a populated import.
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().to_string()
}
