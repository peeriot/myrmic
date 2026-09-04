use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::DynamicReceiver;
use zenoh_traits::nano::ZNSession;
use zenoh_traits::{SendPayload as _, Sender as _, Session as _, Write as _};

use crate::TOPIC_LOGS;
use crate::record::TelemetryRecord;

const SERIALIZE_BUF_SIZE: usize = 512;

/// Publishes telemetry records as JSON to [`TOPIC_LOGS`] on the provided zenoh session.
///
/// Records that fail to serialize or publish are silently dropped.
#[embassy_executor::task]
pub async fn telemetry_task(
    session: ZNSession<'static, NoopRawMutex>,
    receiver: DynamicReceiver<'static, TelemetryRecord>,
) {
    let Ok(mut publisher) = session.publish(TOPIC_LOGS).await else {
        return;
    };

    let mut buf = [0u8; SERIALIZE_BUF_SIZE];

    loop {
        let record = receiver.receive().await;
        let Ok(actual_slice) = postcard::to_slice(&record, &mut buf) else {
            continue;
        };
        let len = actual_slice.len();

        let Ok(payload) = publisher.send().await else {
            continue;
        };
        let Ok(mut writer) = payload.with_encoding("").await else {
            continue;
        };

        let _ = writer.write_all(&buf[..len]).await;
        let _ = zenoh_traits::Close::close(&mut writer).await;
    }
}
