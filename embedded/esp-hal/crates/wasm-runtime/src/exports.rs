//! WASM Exports

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::mem::MaybeUninit;

use myrmic_common::cells::{Command, Event};
use wamr_rust_sdk::module::Module;
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::wasm_export_t;

#[derive(Debug)]
struct Export {
    name: String,
    typ: ExportType,
}

#[derive(Debug, PartialEq)]
#[repr(u32)]
enum ExportType {
    Function = 0,
    Table = 1,
    Memory = 2,
    Global = 3,
}

impl From<core::ffi::c_uint> for ExportType {
    fn from(kind: core::ffi::c_uint) -> ExportType {
        match kind {
            kind if kind as core::ffi::c_uint
                == sys::wasm_import_export_kind_t_WASM_IMPORT_EXPORT_KIND_FUNC =>
            {
                ExportType::Function
            }
            kind if kind as core::ffi::c_uint
                == sys::wasm_import_export_kind_t_WASM_IMPORT_EXPORT_KIND_TABLE =>
            {
                ExportType::Table
            }
            kind if kind as core::ffi::c_uint
                == sys::wasm_import_export_kind_t_WASM_IMPORT_EXPORT_KIND_MEMORY =>
            {
                ExportType::Memory
            }
            kind if kind as core::ffi::c_uint
                == sys::wasm_import_export_kind_t_WASM_IMPORT_EXPORT_KIND_GLOBAL =>
            {
                ExportType::Global
            }
            #[expect(
                clippy::unreachable,
                reason = "The used version of WAMR doesn't have any other variant"
            )]
            _ => unreachable!("WAMR API changed. Fix the export types matching."),
        }
    }
}

/// Scans the WASM module to find cell events and commands to register
pub(crate) fn get_cell_events_commands(module: &Module<'_>) -> (Vec<Event>, Vec<Command>) {
    // safety: C FFI
    let export_count = unsafe { sys::wasm_runtime_get_export_count(module.get_inner_module()) };

    let mut exports = vec![];

    for export_index in 0..export_count {
        let mut export = MaybeUninit::<wasm_export_t>::uninit();
        // safety: C FFI
        unsafe {
            sys::wasm_runtime_get_export_type(
                module.get_inner_module(),
                export_index,
                export.as_mut_ptr(),
            );
        }
        // safety: the C FFI function has populated the memory
        let raw_export = unsafe { export.assume_init() };
        exports.push(Export {
            #[expect(
                clippy::expect_used,
                reason = "We need to know if memory corruption happened"
            )]
            // safety: we know that this pointer contains actual valid initialized memory
            name: unsafe { CStr::from_ptr(raw_export.name) }
                .to_owned()
                .into_string()
                .expect("Malformed export. Memory corruption?"),
            typ: ExportType::from(raw_export.kind),
        });
    }

    // Keep just functions

    let events = exports
        .iter()
        .filter(|export| export.typ == ExportType::Function)
        .filter_map(|export| {
            export
                .name
                .strip_prefix("event_")
                .and_then(|name| Event::new(name.to_owned()).ok())
        })
        .collect();
    let commands = exports
        .iter()
        .filter(|export| export.typ == ExportType::Function)
        .filter_map(|export| {
            export
                .name
                .strip_prefix("command_")
                .and_then(|name| Command::new(name.to_owned()).ok())
        })
        .collect();

    for e in &events {
        log::info!("Found event: {:?}", e);
    }
    for c in &commands {
        log::info!("Found command: {:?}", c);
    }

    (events, commands)
}
