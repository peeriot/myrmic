use std::collections::{BTreeMap, BTreeSet};

use human_bytes::human_bytes;
use swarm_telemetry::db::opentelemetry_proto::tonic::{
    common::v1::{KeyValue, any_value::Value},
    metrics::v1::{Metric, NumberDataPoint, metric::Data, number_data_point},
};

use crate::args::Ctx;

const BOLD_CYAN: &str = "\x1b[1;36m";
const BOLD_GREEN: &str = "\x1b[1;32m";
const DIMMED: &str = "\x1b[2m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

#[derive(Debug, Hash, Ord, PartialOrd, PartialEq, Eq)]
struct Key {
    name: String,
    unit: String,
    // the exporting node's zid, carried as the OTel instrumentation scope name (see
    // `InstrumentationScope::builder(session.zid().to_string())` in
    // `swarm/src/plugins/introspection/run.rs`). Without this in the key, `system.*` metrics
    // (which carry no data-point attributes) from every host collapse onto a single map entry,
    // silently showing only the last-written host's value instead of one line per host.
    host: Option<String>,
    attributes: BTreeSet<String>,
}

fn parse_attributes(kvs: &[KeyValue]) -> BTreeSet<String> {
    kvs.iter()
        .map(|kv| {
            let value = match kv.value.as_ref().and_then(|any| any.value.as_ref()) {
                Some(Value::StringValue(v)) => v.clone(),
                Some(Value::BoolValue(true)) => "TRUE".to_string(),
                Some(Value::BoolValue(false)) => "FALSE".to_string(),
                Some(Value::IntValue(v)) => v.to_string(),
                Some(Value::DoubleValue(v)) => v.to_string(),
                _ => String::new(),
            };
            format!("{}={value}", kv.key)
        })
        .collect()
}

fn format_dp_value(value: number_data_point::Value, unit: &str) -> String {
    if unit == "By" {
        match value {
            number_data_point::Value::AsInt(v) => {
                // only the i64 with max 52 bits can be converted without precision loss. that is around
                // 8 petabytes. bytes metrics record disk IO and memory usage. the precision loss should
                // be fine for the near future
                #[allow(clippy::cast_precision_loss)]
                human_bytes(v as f64)
            }
            number_data_point::Value::AsDouble(v) => human_bytes(v),
        }
    } else {
        match value {
            number_data_point::Value::AsInt(v) => v.to_string(),
            number_data_point::Value::AsDouble(v) => format!("{v:.3}"),
        }
    }
}

fn insert_data_points(
    latest: &mut BTreeMap<Key, String>,
    name: &str,
    unit: &str,
    host: Option<&str>,
    data_points: &[NumberDataPoint],
) {
    for dp in data_points {
        let Some(value) = dp.value else { continue };
        let key = Key {
            name: name.to_string(),
            unit: unit.to_string(),
            host: host.map(str::to_string),
            attributes: parse_attributes(&dp.attributes),
        };
        latest.insert(key, format_dp_value(value, unit));
    }
}

pub async fn handle(
    _ctx: Ctx,
    _cmd: super::Metrics,
    db_client: db_client::v1::Client,
) -> anyhow::Result<()> {
    let entities =
        super::query_telemetry_data::<Metric>(db_client, swarm_telemetry::db::TABLE_METRICS_LATEST)
            .await?;

    let mut latest = BTreeMap::new();
    for (_id, scoped_metric) in entities {
        let name = &scoped_metric.data.name;
        let unit = &scoped_metric.data.unit;
        let host = scoped_metric.scope_name.as_deref();

        let Some(data) = scoped_metric.data.data else {
            continue;
        };

        match data {
            Data::Gauge(gauge) => {
                if let Some(dp) = gauge.data_points.last() {
                    insert_data_points(&mut latest, name, unit, host, std::slice::from_ref(dp));
                }
            }
            Data::Sum(sum) => {
                insert_data_points(&mut latest, name, unit, host, &sum.data_points);
            }
            _ => continue,
        }
    }

    for (key, value) in latest {
        println!("{BOLD_CYAN}{}{RESET}  {BOLD_GREEN}{value}{RESET}", key.name);
        if let Some(host) = &key.host {
            println!("  {DIMMED}host={RESET}{YELLOW}{host}{RESET}");
        }
        for attr in key.attributes {
            if let Some((k, v)) = attr.split_once('=') {
                println!("  {DIMMED}{k}={RESET}{YELLOW}{v}{RESET}");
            } else {
                println!("  {YELLOW}{attr}{RESET}");
            }
        }
    }

    Ok(())
}
