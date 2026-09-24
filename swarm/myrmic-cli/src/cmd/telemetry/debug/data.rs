use std::time::{Duration, SystemTime};

use uuid::Uuid;

/// extracts the insertion time from a row id, assuming it's a `UUIDv7`, which
/// should be the case messages and events, returns `None` otherwise.
pub(crate) fn insertion_time(id: &[u8]) -> Option<SystemTime> {
    let uuid = Uuid::from_slice(id).ok()?;
    let ts = uuid.get_timestamp()?;
    let (secs, nanos) = ts.to_unix();
    Some(SystemTime::UNIX_EPOCH + Duration::new(secs, nanos))
}

/// Debug items with fixed contents, for tests that only care about ordering and output.
#[cfg(test)]
pub(crate) mod test_items {
    use std::time::{Duration, SystemTime};

    use cell_protocol::Sri;
    use myrmic_common::cells::{Command, Event};
    use swarm_telemetry::debug::{DebugCommand, DebugEvent, DebugItem, DebugPayload};

    /// A command addressed to `receiver`, recorded as inserted at `millis` since the epoch.
    pub(crate) fn command_at(millis: u64, receiver: Sri) -> DebugItem {
        DebugItem::Command(DebugCommand {
            trace_id: None,
            inserted_at: SystemTime::UNIX_EPOCH + Duration::from_millis(millis),
            receiver_sri: receiver,
            cmd: Command::try_from("increment").expect("a valid command name"),
            payload: None,
        })
    }

    /// An event recorded as inserted at `millis` since the epoch.
    pub(crate) fn event_at(millis: u64) -> DebugItem {
        DebugItem::Event(DebugEvent {
            trace_id: None,
            inserted_at: SystemTime::UNIX_EPOCH + Duration::from_millis(millis),
            event_name: Event::try_from("rain").expect("a valid event name"),
            payload: DebugPayload::String("20".to_owned()),
        })
    }
}
