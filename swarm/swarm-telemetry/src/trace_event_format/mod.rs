//! simplified implementation of [Trace Event Format](https://docs.google.com/document/d/1CvAClvFfyA5R-PhYUmn5OOQtYMH4h6I0nSsKchNAySU/preview?tab=t.0#heading=h.yr4qxyxotyw)
//! to export `OTel` traces in the a format that can be analyzed in any browser.

use std::collections::HashMap;

use opentelemetry_proto::tonic::{
    common::v1::{KeyValue, any_value::Value},
    trace::v1::Span,
};
use serde::Serialize;
use uuid::Uuid;

use crate::{NO_PARENT_SPAN_ID, db::ScopedEntry};

const NANO_TO_MICRO: u64 = 1_000;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Event {
    #[serde(rename = "pid")]
    pub trace_id: String,
    #[serde(rename = "tid")]
    pub span_id: String,
    #[serde(rename = "ph")]
    pub event_type: EventType,
    #[serde(rename = "cat")]
    pub category: String,
    pub name: Option<String>,
    #[serde(rename = "ts")]
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Args>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
    #[serde(rename = "bp", skip_serializing_if = "Option::is_none")]
    pub binding_point: Option<BindingPoint>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum EventType {
    #[serde(rename = "B")]
    #[default]
    Begin,
    #[serde(rename = "E")]
    End,
    #[serde(rename = "s")]
    FlowStart,
    #[serde(rename = "f")]
    FlowEnd,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum BindingPoint {
    #[serde(rename = "e")]
    #[default]
    End,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Args {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    #[serde(rename = "code.file.path")]
    pub code_file_path: Option<String>,
    #[serde(rename = "code.line.number")]
    pub code_line_number: Option<u32>,
    pub module_id: Option<String>,
    pub message_type: Option<String>,
    pub thread_id: Option<u32>,
}

impl std::cmp::Ord for Event {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // the main order is defined by the timestamp
        self.timestamp.cmp(&other.timestamp).then_with(|| {
            // perfetto requires that flow start and end are "enclosed" by the even begin
            // and end. we use the flow to display the time that passed between a message
            // has processed in cell A and the processing is starting in cell B (in case
            // they are related). therefore the flow start timestamp is equal to the event
            // end timestamp of the source cell and the flow end timestamp is equal to the
            // event begin timestamp of the destination cell.
            // as perfetto requires the events to "enclose" the flow we add a tie breaker
            // here for the situation mentioned above.
            match (&self.event_type, &other.event_type) {
                (EventType::FlowStart, EventType::End) | (EventType::Begin, EventType::FlowEnd) => {
                    std::cmp::Ordering::Less
                }
                (EventType::End, EventType::FlowStart) | (EventType::FlowEnd, EventType::Begin) => {
                    std::cmp::Ordering::Greater
                }
                _ => std::cmp::Ordering::Equal,
            }
        })
    }
}

impl std::cmp::PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub fn process_spans(
    spans: impl Iterator<Item = (Uuid, ScopedEntry<Span>)>,
    filter_trace_id: Option<Uuid>,
) -> Vec<Event> {
    let span_map: HashMap<u64, (Span, Uuid)> = spans
        .filter_map(|(eid, span)| {
            let trace_id = span
                .data
                .trace_id
                .as_slice()
                .try_into()
                .ok()
                .map(u128::from_be_bytes)
                .map(Uuid::from_u128);

            let (_, span_id) = eid.as_u64_pair();
            if let Some(trace_id) = trace_id {
                Some((span_id, (span.data, trace_id)))
            } else {
                None
            }
        })
        .collect();

    let mut events = vec![];
    // increasing number for "flows", flowse need to have numbered IDs, just increase for each new flow
    let mut flow_count = 0;

    for (span_id, (span, trace_id)) in span_map
        .iter()
        .filter(|(_, (_, trace_id))| filter_trace_id.is_none_or(|f| f == *trace_id))
    {
        let span_events = process_span(trace_id, *span_id, span, &span_map, &mut flow_count);
        events.extend_from_slice(&span_events);
    }

    events
}

fn extract_str(attr: &KeyValue) -> Option<String> {
    match attr.value.as_ref()?.value.as_ref()? {
        Value::StringValue(v) => Some(v.clone()),
        _ => None,
    }
}
fn extract_u32(attr: &KeyValue) -> Option<u32> {
    match attr.value.as_ref()?.value.as_ref()? {
        Value::IntValue(v) => u32::try_from(*v).ok(),
        _ => None,
    }
}

fn encode_base32(data: &[u8]) -> String {
    base32::encode(base32::Alphabet::Rfc4648Lower { padding: false }, data)
}

pub fn process_span<H: std::hash::BuildHasher>(
    trace_id: &Uuid,
    span_id: u64,
    span: &Span,
    span_map: &HashMap<u64, (Span, Uuid), H>,
    flow_count: &mut u32,
) -> Vec<Event> {
    let Span {
        trace_id: _,
        span_id: _,
        trace_state: _,
        parent_span_id,
        flags: _,
        name,
        kind: _,
        start_time_unix_nano,
        end_time_unix_nano,
        attributes,
        dropped_attributes_count: _,
        events: _,
        dropped_events_count: _,
        links: _,
        dropped_links_count: _,
        status: _,
    } = span;

    let parent_span_id = parent_span_id
        .as_slice()
        .try_into()
        .map(u64::from_be_bytes)
        .unwrap_or_default();

    // base32 encode
    let base32_span_id = encode_base32(&span_id.to_be_bytes());
    let base32_parent_span_id = encode_base32(&parent_span_id.to_be_bytes());

    let mut code_file_path = None;
    let mut code_line_number = None;
    let mut module_id = None;
    let mut message_type = None;
    let mut thread_id = None;
    let mut identifier = None;

    for attribute in attributes {
        match attribute.key.as_str() {
            "code.file.path" => code_file_path = extract_str(attribute),
            "code.line.number" => code_line_number = extract_u32(attribute),
            "module_id" => module_id = extract_str(attribute),
            "message_type" => message_type = extract_str(attribute),
            "thread.id" => thread_id = extract_u32(attribute),
            "identifier" => identifier = extract_str(attribute),
            _ => {}
        }
    }

    let trace_id = trace_id.as_simple().to_string();
    // build args
    let args = Args {
        trace_id: trace_id.clone(),
        span_id: base32_span_id.clone(),
        parent_span_id: (parent_span_id != NO_PARENT_SPAN_ID)
            .then(|| base32_parent_span_id.clone()),
        code_file_path,
        code_line_number,
        module_id,
        message_type,
        thread_id,
    };

    // start event
    let start_event = Event {
        trace_id: trace_id.clone(),
        span_id: base32_span_id.clone(),
        event_type: EventType::Begin,
        category: name.clone(),
        name: identifier.clone(),
        timestamp: start_time_unix_nano / NANO_TO_MICRO,
        args: Some(args),
        ..Default::default()
    };

    // end event
    let end_event = Event {
        event_type: EventType::End,
        timestamp: end_time_unix_nano / NANO_TO_MICRO,
        args: None,
        ..start_event.clone()
    };

    if let Some((parent_span, _)) = span_map.get(&parent_span_id) {
        *flow_count = flow_count.saturating_add(1);
        let id = Some(*flow_count);

        let flow_start = Event {
            trace_id: trace_id.clone(),
            span_id: base32_parent_span_id,
            event_type: EventType::FlowStart,
            timestamp: parent_span.end_time_unix_nano / NANO_TO_MICRO,
            args: None,
            binding_point: Some(BindingPoint::End),
            id,
            category: name.clone(),
            name: Some("network".into()),
        };

        let flow_end = Event {
            trace_id,
            span_id: base32_span_id,
            event_type: EventType::FlowEnd,
            timestamp: start_time_unix_nano / NANO_TO_MICRO,
            args: None,
            binding_point: Some(BindingPoint::End),
            id,
            category: name.clone(),
            name: Some("network".into()),
        };

        vec![start_event, flow_start, flow_end, end_event]
    } else {
        vec![start_event, end_event]
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;

    use itertools::Itertools;
    use opentelemetry_proto::tonic::trace::v1::Span;
    use uuid::Uuid;

    #[test]
    fn test_enclosing_order() {
        let source_begin = super::Event {
            event_type: super::EventType::Begin,
            timestamp: 0,
            ..Default::default()
        };
        let source_end = super::Event {
            event_type: super::EventType::End,
            timestamp: 1,
            ..Default::default()
        };

        let dest_begin = super::Event {
            event_type: super::EventType::Begin,
            timestamp: 10,
            ..Default::default()
        };
        let dest_end = super::Event {
            event_type: super::EventType::End,
            timestamp: 11,
            ..Default::default()
        };

        let flow_start = super::Event {
            event_type: super::EventType::FlowStart,
            timestamp: 1,
            ..Default::default()
        };
        let flow_end = super::Event {
            event_type: super::EventType::FlowEnd,
            timestamp: 10,
            ..Default::default()
        };

        let expected_order = vec![
            source_begin,
            flow_start,
            source_end,
            dest_begin,
            flow_end,
            dest_end,
        ];

        // generate all permutations and ensure the expected order is achieved afterwards
        for ordering in (0..6).permutations(6) {
            let mut test_vec = vec![];
            for i in ordering {
                test_vec.push(expected_order[i].clone());
            }

            test_vec.sort();
            assert_eq!(expected_order, test_vec);
        }
    }

    fn span(start_ns: u64, end_ns: u64, parent_id: u64) -> Span {
        Span {
            parent_span_id: parent_id.to_be_bytes().to_vec(),
            start_time_unix_nano: start_ns,
            end_time_unix_nano: end_ns,
            ..Default::default()
        }
    }

    #[test]
    fn process_root_span() {
        let span_map = HashMap::new();
        let mut flow_count = 0;
        let trace_id = Uuid::from_u128(1);

        let mut events = super::process_span(
            &trace_id,
            0,
            &span(0, 10_500, 0),
            &span_map,
            &mut flow_count,
        );
        events.sort();

        assert_eq!(2, events.len());
        assert_eq!(super::EventType::Begin, events[0].event_type);
        assert_eq!(0, events[0].timestamp);
        assert_eq!(super::EventType::End, events[1].event_type);
        assert_eq!(10, events[1].timestamp);
    }

    #[test]
    fn process_child_span() {
        let mut span_map = HashMap::new();
        let mut flow_count = 0;
        let trace_id = Uuid::from_u128(1);

        span_map.insert(1u64, (span(0, 10_500, 0), trace_id));

        let mut events = super::process_span(
            &trace_id,
            0,
            &span(20_250, 30_750, 1),
            &span_map,
            &mut flow_count,
        );
        events.sort();

        assert_eq!(4, events.len());
        assert_eq!(super::EventType::FlowStart, events[0].event_type);
        assert_eq!(10, events[0].timestamp);
        assert_eq!(super::EventType::Begin, events[1].event_type);
        assert_eq!(20, events[1].timestamp);
        assert_eq!(super::EventType::FlowEnd, events[2].event_type);
        assert_eq!(20, events[2].timestamp);
        assert_eq!(super::EventType::End, events[3].event_type);
        assert_eq!(30, events[3].timestamp);
    }

    #[test]
    fn process_span_preserves_microsecond_precision() {
        let span_map = HashMap::new();
        let mut flow_count = 0;
        let trace_id = Uuid::from_u128(1);

        let mut events = super::process_span(
            &trace_id,
            0,
            &span(1_234, 2_345, 0),
            &span_map,
            &mut flow_count,
        );
        events.sort();

        assert_eq!(2, events.len());
        assert_eq!(1, events[0].timestamp);
        assert_eq!(2, events[1].timestamp);
    }
}
