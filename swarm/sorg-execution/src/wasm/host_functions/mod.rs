use std::future::Future;

use errors::report_error;
use logging::log;
use myrmic_common::types::error::GENERIC_ERROR;
use wasmtime::{Caller, Linker, Memory};

use crate::{
    Result,
    wasm::{
        cell::state::CellState,
        host_functions::{
            arguments::get_arguments,
            cell::{
                cancel_timer, create_timer, publish_event, send_command, spawn_cell, stop_self,
                terminate_cell,
            },
            db::{
                blob_link, blob_move, blob_resolve, blob_store, blob_unlink, find_measurement,
                key_delete, key_get, key_prefix, key_put, path_resolve, paths_list,
                publish_measurement, sem_select, sem_update, tb_append, tb_count, tb_delete,
                tb_get, tb_insert, tb_list,
            },
            gateway::{gateway_mount, gateway_unmount},
            time::{now_host, uptime_host, wait_host},
        },
    },
};

mod arguments;
#[cfg(feature = "ble-linux")]
mod ble;
mod cell;
mod db;
mod errors;
mod gateway;
mod logging;
mod outlet;
mod sl_claim;
mod tap;
mod time;

pub use outlet::link_outlet_functions;
pub use sl_claim::{CellIdentity, release_sl_claim};
pub use tap::link_tap_functions;

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

pub use crate::tri;

pub(crate) fn link_cell_functions(linker: &mut Linker<CellState>) -> Result<()> {
    #[cfg(feature = "ble-linux")]
    link_ble_functions(linker)?;

    link_db_functions(linker)?;

    link_gateway_functions(linker)?;

    linker.func_wrap("time", "now_host", now_host)?;
    linker.func_wrap("time", "uptime_host", uptime_host)?;
    linker.func_wrap_async(
        "time",
        "wait_host",
        move |caller: Caller<'_, _>, (buffer_ptr, length): (u32, u32)| {
            Box::new(wait_host(caller, buffer_ptr, length)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap("logging", "log_host", log)?;

    linker.func_wrap("error", "report_error", report_error)?;

    linker.func_wrap("arguments", "get_arguments", get_arguments)?;

    linker.func_wrap_async(
        "cell",
        "send_command",
        move |caller: Caller<'_, CellState>, (buf_ptr, len): (u32, u32)| {
            Box::new(send_command(caller, buf_ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "cell",
        "publish_event",
        move |caller: Caller<'_, CellState>, (buf_ptr, len): (u32, u32)| {
            Box::new(publish_event(caller, buf_ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "cell",
        "spawn_cell_host",
        move |caller: Caller<'_, CellState>, (buf_ptr, len, out_sri): (u32, u32, u32)| {
            Box::new(spawn_cell(caller, buf_ptr, len, out_sri))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "cell",
        "terminate_cell_host",
        move |caller: Caller<'_, CellState>, (buf_ptr, len): (u32, u32)| {
            Box::new(terminate_cell(caller, buf_ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "cell",
        "stop_self_host",
        move |caller: Caller<'_, CellState>, (code_present, code): (u32, u32)| {
            Box::new(stop_self(caller, code_present, code)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "cell",
        "create_timer",
        move |caller: Caller<'_, CellState>, (buf_ptr, len): (u32, u32)| {
            Box::new(create_timer(caller, buf_ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "cell",
        "cancel_timer",
        move |caller: Caller<'_, CellState>, (id,): (u32,)| {
            Box::new(cancel_timer(caller, id)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    Ok(())
}

fn link_gateway_functions(linker: &mut Linker<CellState>) -> Result<()> {
    /* Gateway routing functions */

    linker.func_wrap_async(
        "gateway",
        "gateway_mount",
        move |caller: Caller<'_, CellState>, (ptr, len): (u32, u32)| {
            Box::new(gateway_mount(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "gateway",
        "gateway_unmount",
        move |caller: Caller<'_, CellState>, (ptr, len): (u32, u32)| {
            Box::new(gateway_unmount(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    Ok(())
}

fn link_blob_functions(linker: &mut Linker<CellState>) -> Result<()> {
    /* Blob functions */

    linker.func_wrap_async(
        "db",
        "blob_store",
        move |caller: Caller<'_, _>,
              (ptr_req, len_req, ptr_blob, len_blob, ptr_rsp, len_rsp): (
            u32,
            u32,
            u32,
            u32,
            u32,
            u32,
        )| {
            Box::new(blob_store(
                caller, ptr_req, len_req, ptr_blob, len_blob, ptr_rsp, len_rsp,
            )) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "blob_link",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(blob_link(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "blob_unlink",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(blob_unlink(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "blob_move",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(blob_move(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "blob_resolve",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(blob_resolve(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "path_resolve",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(path_resolve(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "paths_list",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(paths_list(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    Ok(())
}

fn link_db_functions(linker: &mut Linker<CellState>) -> Result<()> {
    /* DB host functions */

    link_blob_functions(linker)?;

    /* Time series functions */

    linker.func_wrap_async(
        "db",
        "publish_measurement",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(publish_measurement(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "find_measurement",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(find_measurement(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    /* Key-value functions */

    linker.func_wrap_async(
        "db",
        "key_put",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(key_put(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "key_delete",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(key_delete(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "key_get",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(key_get(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "key_prefix",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(key_prefix(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    /* Table functions */

    linker.func_wrap_async(
        "db",
        "tb_insert",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(tb_insert(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "tb_count",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(tb_count(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "tb_get",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(tb_get(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "tb_list",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(tb_list(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "tb_append",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(tb_append(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "tb_delete",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(tb_delete(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    link_sem_functions(linker)?;

    Ok(())
}

fn link_sem_functions(linker: &mut Linker<CellState>) -> Result<()> {
    /* Semantic functions */

    linker.func_wrap_async(
        "db",
        "sem_update",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(sem_update(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "db",
        "sem_select",
        move |caller: Caller<'_, _>, (ptr_req, len_req, ptr_rsp, len_rsp): (u32, u32, u32, u32)| {
            Box::new(sem_select(caller, ptr_req, len_req, ptr_rsp, len_rsp))
                as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    Ok(())
}

#[cfg(feature = "ble-linux")]
fn link_ble_functions(linker: &mut Linker<CellState>) -> Result<()> {
    use ble::{
        connect, disconnect, read, scan, set_pair_passkey, stop_scan, subscribe, unsubscribe, write,
    };

    /* BLE host functions (callback-oriented ABI) */
    linker.func_wrap_async(
        "ble",
        "ble_scan",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(scan(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "ble",
        "ble_stop_scan",
        move |caller: Caller<'_, _>, (): ()| {
            Box::new(stop_scan(caller)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "ble",
        "ble_connect",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(connect(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "ble",
        "ble_disconnect",
        move |caller: Caller<'_, _>, (id,): (u32,)| {
            Box::new(disconnect(caller, id)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "ble",
        "ble_subscribe",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(subscribe(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "ble",
        "ble_unsubscribe",
        move |caller: Caller<'_, _>, (id,): (u32,)| {
            Box::new(unsubscribe(caller, id)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "ble",
        "ble_read",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(read(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "ble",
        "ble_write",
        move |caller: Caller<'_, _>, (ptr, len): (u32, u32)| {
            Box::new(write(caller, ptr, len)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    linker.func_wrap_async(
        "ble",
        "ble_set_pair_passkey",
        move |caller: Caller<'_, _>, (passkey,): (u32,)| {
            Box::new(set_pair_passkey(caller, passkey)) as Box<dyn Future<Output = i32> + Send>
        },
    )?;

    Ok(())
}

fn get_memory<T>(caller: &mut Caller<'_, T>) -> Memory {
    caller
        .get_export("memory")
        .expect("module memory checked during module load")
        .into_memory()
        .expect("module memory checked during module load")
}

fn as_slice<'a, T>(caller: &'a mut Caller<'_, T>, ptr: usize, len: usize) -> &'a [u8] {
    let memory = get_memory(caller);
    &memory.data(&*caller)[ptr..(ptr + len)]
}

fn as_slice_mut<'a, T>(caller: &'a mut Caller<'_, T>, ptr: usize, len: usize) -> &'a mut [u8] {
    let memory = get_memory(caller);
    &mut memory.data_mut(&mut *caller)[ptr..(ptr + len)]
}

fn decode<T: serde::de::DeserializeOwned>(
    caller: &mut Caller<'_, CellState>,
    payload_ptr: u32,
    payload_len: u32,
    context: &str,
) -> std::result::Result<T, i32> {
    let data = as_slice(caller, payload_ptr as usize, payload_len as usize);

    match postcard::take_from_bytes::<T>(data) {
        Ok((ty, _)) => Ok(ty),
        Err(err) => {
            tracing::error!("failed to deserialize {}: {}", context, err);
            Err(GENERIC_ERROR)
        }
    }
}

fn encode<T: serde::Serialize>(
    caller: &mut Caller<'_, CellState>,
    out_ptr: u32,
    out_len: u32,
    response: &T,
    context: &str,
) -> std::result::Result<(), i32> {
    let data = as_slice_mut(caller, out_ptr as usize, out_len as usize);

    match postcard::to_slice(response, data) {
        Ok(_) => Ok(()),
        Err(err) => {
            tracing::error!("failed to write {}: {}", context, err);
            Err(GENERIC_ERROR)
        }
    }
}
