use std::time::Duration;

use swarm_onboarding::qr::QrPayload;
use swarm_onboarding::{DeviceProfile, OpNetFlags};
use swarm_onboarding_request::{ONBOARDING_REQUEST_TOPIC, OnboardingRequest};
use tokio::task::LocalSet;
use tokio::time::{sleep, timeout};
use zenoh::Session;
use zenoh::pubsub::Publisher;

use super::run_onboarding;

const ONBOARDING_TIMEOUT: Duration = Duration::from_secs(3);

/// The P-256 generator point, SEC1 uncompressed: a valid public key of a
/// device that never answers.
const ABSENT_DEVICE_KEY: [u8; 65] = [
    0x04, 0x6b, 0x17, 0xd1, 0xf2, 0xe1, 0x2c, 0x42, 0x47, 0xf8, 0xbc, 0xe6, 0xe5, 0x63, 0xa4, 0x40,
    0xf2, 0x77, 0x03, 0x7d, 0x81, 0x2d, 0xeb, 0x33, 0xa0, 0xf4, 0xa1, 0x39, 0x45, 0xd8, 0x98, 0xc2,
    0x96, 0x4f, 0xe3, 0x42, 0xe2, 0xfe, 0x1a, 0x7f, 0x9b, 0x8e, 0xe7, 0xeb, 0x4a, 0x7c, 0x0f, 0x9e,
    0x16, 0x2b, 0xce, 0x33, 0x57, 0x6b, 0x31, 0x5e, 0xce, 0xcb, 0xb6, 0x40, 0x68, 0x37, 0xbf, 0x51,
    0xf5,
];

/// The meta topic the installer serves for `ABSENT_DEVICE_KEY`.
const ABSENT_DEVICE_META: &str = "@onboarding/@v1/@-meta/@BGsX0fLhLEJH+Lzm5WOkQPJ3A32BLeszoPShOUXYmMKWT+NC4v4af5uO5+tKfA+eFivOM1drMV7Oy7ZAaDe-UfU";

#[tokio::test(flavor = "multi_thread")]
async fn stalled_onboarding_gives_way_to_the_next_request() {
    let session = open_session().await;

    LocalSet::new()
        .run_until(async {
            tokio::task::spawn_local(run_onboarding(session.clone(), ONBOARDING_TIMEOUT));

            let publisher = session
                .declare_publisher(ONBOARDING_REQUEST_TOPIC)
                .await
                .unwrap();
            wait_for_subscriber(&publisher).await;

            publisher.put(request()).await.unwrap();
            let first = wait_for_meta(&session, None, ONBOARDING_TIMEOUT)
                .await
                .expect("the first request was never served");

            publisher.put(request()).await.unwrap();
            wait_for_meta(&session, Some(&first), Duration::from_secs(15))
                .await
                .expect("the next request was never served");
        })
        .await;
}

/// Multicast scouting is off, so the session only ever talks to itself.
///
/// Opened and dropped outside the `LocalSet`: zenoh blocks in place there.
async fn open_session() -> Session {
    let mut config = zenoh::Config::default();
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .unwrap();

    zenoh::open(config).await.expect("unable to open session")
}

async fn wait_for_subscriber(publisher: &Publisher<'_>) {
    timeout(Duration::from_secs(5), async {
        while !publisher.matching_status().await.unwrap().matching() {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the onboarding subscriber never came up");
}

fn request() -> Vec<u8> {
    let mut buf = [0u8; 256];
    let (profile, buf) = DeviceProfile::new(OpNetFlags::all())
        .serialize(&mut buf)
        .unwrap();
    let (device_qr, _) = QrPayload::new(&ABSENT_DEVICE_KEY, profile)
        .as_str(buf)
        .unwrap();

    serde_json::to_vec(&OnboardingRequest {
        operational_networks: Vec::new(),
        device_qr: device_qr.to_owned(),
    })
    .unwrap()
}

/// Polls the absent device's meta topic until it serves a meta other than
/// `other_than`, or `within` elapses.
async fn wait_for_meta(
    session: &Session,
    other_than: Option<&[u8]>,
    within: Duration,
) -> Option<Vec<u8>> {
    timeout(within, async {
        loop {
            if let Some(meta) = served_meta(session).await
                && Some(meta.as_slice()) != other_than
            {
                return meta;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .ok()
}

async fn served_meta(session: &Session) -> Option<Vec<u8>> {
    let replies = session
        .get(ABSENT_DEVICE_META)
        .timeout(Duration::from_secs(1))
        .await
        .unwrap();
    let reply = replies.recv_async().await.ok()?;

    Some(reply.result().ok()?.payload().to_bytes().into_owned())
}
