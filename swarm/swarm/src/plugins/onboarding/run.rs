use futures::StreamExt;

use swarm_onboarding::io::{NoopConsumer, SerdeObjError, SliceProducer};
use swarm_onboarding::qr::QrPayload;
use swarm_onboarding::zenoh::installer::Installer;
use swarm_onboarding::{
    BufferOverflowError, DeviceCreds, DeviceProfile, InstallerError, OnboardedDevice,
};

use thiserror::Error;
use tokio::sync::oneshot::Receiver;

use tracing::{debug, error, info};

use zenoh::Session;

use zenoh::bytes::ZBytes;
use zenoh_traits::ErrorKind;
use zenoh_traits::zenoh::ZSession;

use swarm_onboarding_request::{Network, ONBOARDING_REQUEST_TOPIC, OnboardingRequest};

pub(super) async fn run(session: Session, poison_rcv: Receiver<()>) {
    // NOTE:
    // The onboarding process is spawned in Tokio's LocalSet
    // because we are seemingly hitting a rustc bug where Rust cannot derive the `Send` auto-trait
    // for futures __internally__ using the `zenoh_traits` crate inside a regular work-stealing `tokio::spawn` context.
    // Which in turn - and as per the comments in the bug - is a side effect from us using associated types in the
    // `zenoh_traits` crate. Which is not having any meaningful alternative though!
    //
    // Most likely we are hitting this bug:
    // https://users.rust-lang.org/t/implementation-of-trait-is-not-general-enough-when-used-inside-tokio-spawn/122490
    // (and yes, as per the comments, rustc does not even talk about missing `Send` bounds in its error messages)
    //
    // If not convinced, try the code below:
    // - Keeping the `Send` bound on the dyn Future fails to typecheck
    // - When removing the `Send` bound, code typechecks, but then `tokio::spawn` would obviously complain
    //   that the future is not `Send` (with the same cryptic error mentioning lifetimes!) if the future is emplaced
    //   in a regular work-stealing `tokio::spawn` context.
    //
    // Note also, that there is _no reason_ why the future below should not be `Send`. It IS `Send` even though rustc cannot derive it:
    // - `Session` is `Send` (it is just an Arc internally)
    // - The impls of all `zenoh_traits` types over `Zenoh` types are `Send`, because they don't really use raw pointers or anything like that
    //   which would make them non-Send
    // {
    //     async fn foo(session: Session) {
    //         bar(&ZSession::new(session)).await;
    //     }

    //     async fn bar<S: zenoh_traits::Session>(session: S) {
    //         use zenoh_traits::Receiver;

    //         let mut receiver = session.get("foo").await.unwrap();
    //         let _ = receiver.receive().await.unwrap();
    //     }

    //     let _: core::pin::Pin<Box<dyn core::future::Future<Output = _> + Send>> = Box::pin(foo(session.clone()));
    // }

    debug!("spawning sorg onboarding");
    match run_onboarding_until(session, poison_rcv).await {
        Ok(()) => debug!("sorg onboarding terminated"),
        Err(err) => error!("sorg onboarding terminated with an error: {err}"),
    }
}

// TODO: Do we really need an enum-based error in the context of a plugin (an app, rather than a library)?
// `anyhow` would do just as well?
#[derive(Error, Debug)]
enum OnboardingError {
    #[error("invalid onboarding request: {0:?}")]
    InvalidOnboardingRequest(#[from] serde_json::Error),
    #[error("buffer overflow")]
    BufferOverflow(#[from] BufferOverflowError),
    #[error("invalid QR code: {0}")]
    InvalidQrCode(#[from] SerdeObjError),
    #[error("installer error: {0}")]
    OnboardingInstaller(#[from] InstallerError<ErrorKind, ErrorKind>),
    #[error("zenoh error: {0}")]
    Zenoh(#[from] Box<dyn core::error::Error + Send + Sync>),
}

async fn run_onboarding_until(
    session: Session,
    off_rcv: Receiver<()>,
) -> Result<(), OnboardingError> {
    info!("Spawning swarm-onboarding");

    tokio::select! {
        _ = run_onboarding(session) => {
            info!("Onboarding task completed");
        },
        _ = off_rcv => {
            info!("Onboarding received shutdown signal");
        }
    }

    Ok(())
}

async fn run_onboarding(session: Session) -> Result<(), OnboardingError> {
    let subscriber = session.declare_subscriber(ONBOARDING_REQUEST_TOPIC).await?;

    let mut onboarding_pending = subscriber.stream();

    while let Some(next) = onboarding_pending.next().await {
        let payload = next.payload();

        if let Err(r) = process_onboarding_req(&session, payload).await {
            error!("Failed to process onboarding request: {r}");
        }
    }

    Ok(())
}

async fn process_onboarding_req(
    session: &Session,
    payload: &ZBytes,
) -> Result<(), OnboardingError> {
    info!("Got onboarding request");

    let onboarding_request = serde_json::from_reader::<_, OnboardingRequest>(payload.reader())?;

    info!("Onboarding request: {:?}", onboarding_request);

    let mut buf = vec![0; 16384];
    let (qr_payload, buf) = QrPayload::decode(&onboarding_request.device_qr, &mut buf)?;

    let device_profile = DeviceProfile::deserialize(qr_payload.device_profile)?;
    info!("Device profile: {}", device_profile);

    let onboarded_device = OnboardedDevice::new(
        DeviceCreds::PKI {
            pub_key: qr_payload.device_creds,
        },
        device_profile,
    );

    let (device_id, buf) = onboarded_device.device_id(buf)?;

    info!("Onboarded device ID: {device_id}");

    onboard(
        session,
        &onboarded_device,
        &onboarding_request.operational_networks,
        buf,
    )
    .await?;

    Ok(())
}

async fn onboard(
    session: &Session,
    onboarded_device: &OnboardedDevice<'_>,
    operational_networks: &[Network],
    buf: &mut [u8],
) -> Result<(), InstallerError<ErrorKind, ErrorKind>> {
    // Should never fail
    let networks_payload = serde_json::to_string(operational_networks)
        .expect("failed to serialize operational networks");

    let mut installer = Installer::new(
        ZSession::new(session.clone()),
        // For now, we are not interested in the data of the `DeviceAttestation` message
        NoopConsumer,
        // For now, the `OnboardingData` message will just contain the operational networks as a JSON string
        SliceProducer::new(networks_payload.as_bytes()),
    );

    // Installer needs a proper CryptoRng impl
    let mut rng = rand_08::rngs::OsRng;

    installer.onboard(onboarded_device, &mut rng, buf).await
}
