//! Time-series datalayer host functions.

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;

use db_client::v1::models::{
    FieldValue as DbFieldValue, TsOrderBy as DbTsOrderBy, ts_find, ts_publish,
};
use myrmic_common::db::{
    FieldValue as WasmFieldValue, FindRequest, FindResponse, Measurement, PublishRequest, Sample,
    TsOrderBy as WasmTsOrderBy,
};
use wasm_runtime_macros::host_function;

use crate::imports::db::{apply, decode_request, defer, encode_response, transform_scope};
use crate::tri;

fn to_db_field_value(value: WasmFieldValue) -> DbFieldValue {
    match value {
        WasmFieldValue::I64(i) => DbFieldValue::I64(i),
        WasmFieldValue::U64(u) => DbFieldValue::U64(u),
        WasmFieldValue::F64(f) => DbFieldValue::F64(f),
        WasmFieldValue::String(s) => DbFieldValue::String(s),
        WasmFieldValue::Boolean(b) => DbFieldValue::Boolean(b),
    }
}

fn to_db_order(order: WasmTsOrderBy) -> DbTsOrderBy {
    match order {
        WasmTsOrderBy::TimestampAsc => DbTsOrderBy::TimestampAsc,
        WasmTsOrderBy::TimestampDesc => DbTsOrderBy::TimestampDesc,
    }
}

fn to_wasm_field_value(value: DbFieldValue) -> WasmFieldValue {
    match value {
        DbFieldValue::I64(i) => WasmFieldValue::I64(i),
        DbFieldValue::U64(u) => WasmFieldValue::U64(u),
        DbFieldValue::F64(f) => WasmFieldValue::F64(f),
        DbFieldValue::String(s) => WasmFieldValue::String(s),
        DbFieldValue::Boolean(b) => WasmFieldValue::Boolean(b),
    }
}

#[host_function]
fn publish_measurement(buffer: *const u8, length: c_int) -> c_int {
    let PublishRequest { scope, measurement } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(buffer, length, "time-series publish request") }
    );

    let Measurement {
        name,
        tags,
        fields,
        ts,
    } = measurement;

    let fields = fields
        .into_iter()
        .map(|(k, v)| (k, to_db_field_value(v)))
        .collect();
    // The esp host has no wall clock, so a missing timestamp is left as 0 for the modem/db to
    // resolve later.
    let timestamp = ts.unwrap_or(0);

    let scope = tri!(transform_scope(scope));

    defer(ts_publish::Op {
        scope,
        measurement: name,
        tags,
        fields,
        timestamp,
    })
}

#[host_function]
fn find_measurement(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let FindRequest {
        scope,
        measurement_name,
        limit,
        start,
        end,
        order,
    } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "time-series find request") }
    );

    let scope = tri!(transform_scope(scope));

    let ts_find::Response { samples } = tri!(apply(ts_find::Op {
        scope,
        measurement: measurement_name,
        limit,
        start,
        end,
        order: order.map(to_db_order),
    }));

    let samples: Vec<Sample> = samples
        .into_iter()
        .map(|(tags, fields, timestamp)| {
            let fields: Vec<(String, WasmFieldValue)> = fields
                .into_iter()
                .map(|(k, v)| (k, to_wasm_field_value(v)))
                .collect();
            Sample {
                tags,
                fields,
                timestamp,
            }
        })
        .collect();

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &FindResponse { samples },
            "time-series find response",
        )
    }
}
