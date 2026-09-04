use db_client::v1::models::ts_find::Response;
use myrmic_common::db::{FindRequest, FindResponse, Measurement, PublishRequest, Sample};
use myrmic_common::types::error::SUCCESS;
use wasmtime::Caller;

use db_client::v1::models::FieldValue as DbFieldValue;
use db_client::v1::models::TsOrderBy as DbTsOrderBy;
use db_client::v1::models::{ts_find, ts_publish};
use myrmic_common::db::FieldValue as WasmFieldValue;
use myrmic_common::db::TsOrderBy as WasmTsOrderBy;

use crate::wasm::cell::state::CellState;
use crate::wasm::host_functions::{
    db::{apply, defer, transform_scope},
    decode, encode, tri,
};

pub(crate) async fn publish_measurement(
    mut caller: Caller<'_, CellState>,
    payload_ptr: u32,
    payload_len: u32,
) -> i32 {
    let PublishRequest { scope, measurement } = tri!(decode(
        &mut caller,
        payload_ptr,
        payload_len,
        "time-series publish request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));

    let Measurement {
        name,
        tags,
        fields,
        ts,
    } = measurement;
    let fields = transform_fields(fields);
    let ts = ts.unwrap_or_else(|| caller.data().session().new_timestamp().get_time().as_u64());

    defer(
        &mut caller,
        ts_publish::Op {
            scope,
            measurement: name,
            tags,
            fields,
            timestamp: ts,
        },
    )
}

pub(crate) async fn find_measurement(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let FindRequest {
        scope,
        measurement_name,
        limit,
        start,
        end,
        order,
    } = tri!(decode(
        &mut caller,
        req_ptr,
        req_len,
        "time-series find request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));
    let order = order.map(transform_order);

    let response = tri!(
        apply(
            &mut caller,
            ts_find::Op {
                scope,
                measurement: measurement_name,
                limit,
                start,
                end,
                order,
            },
        )
        .await
    );
    let find_response = transform_response(response);

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &find_response,
        "time-series find response"
    ));

    SUCCESS
}

fn transform_fields(fields: Vec<(String, WasmFieldValue)>) -> Vec<(String, DbFieldValue)> {
    fields
        .into_iter()
        .map(|(k, v)| (k, transform_field_value(v)))
        .collect()
}

fn transform_order(order: WasmTsOrderBy) -> DbTsOrderBy {
    match order {
        WasmTsOrderBy::TimestampAsc => DbTsOrderBy::TimestampAsc,
        WasmTsOrderBy::TimestampDesc => DbTsOrderBy::TimestampDesc,
    }
}

fn transform_field_value(f_value: WasmFieldValue) -> DbFieldValue {
    match f_value {
        WasmFieldValue::I64(i) => DbFieldValue::I64(i),
        WasmFieldValue::U64(u) => DbFieldValue::U64(u),
        WasmFieldValue::F64(f) => DbFieldValue::F64(f),
        WasmFieldValue::String(s) => DbFieldValue::String(s),
        WasmFieldValue::Boolean(b) => DbFieldValue::Boolean(b),
    }
}

fn transform_response(response: Response) -> FindResponse {
    let wasm_samples = response
        .samples
        .into_iter()
        .map(|(tags, fields, ts)| {
            let fields: Vec<_> = fields
                .into_iter()
                .map(|(k, v)| {
                    let value = match v {
                        DbFieldValue::I64(i) => WasmFieldValue::I64(i),
                        DbFieldValue::U64(u) => WasmFieldValue::U64(u),
                        DbFieldValue::F64(f) => WasmFieldValue::F64(f),
                        DbFieldValue::String(s) => WasmFieldValue::String(s),
                        DbFieldValue::Boolean(b) => WasmFieldValue::Boolean(b),
                    };

                    (k, value)
                })
                .collect();

            Sample {
                tags,
                fields,
                timestamp: ts,
            }
        })
        .collect();

    FindResponse {
        samples: wasm_samples,
    }
}
