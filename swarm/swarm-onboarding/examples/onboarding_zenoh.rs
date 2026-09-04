//! Example of device onboarding over Zenoh.
//!
//! The Zenoh implementation is using in-memory pipes for communication between the device and the installer.

#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

extern crate alloc;

use embassy_executor::{Executor, Spawner};
use embassy_futures::join::join;
use embassy_time::Duration;

use log::info;

use static_cell::StaticCell;

use swarm_onboarding::io::{SliceConsumer, SliceProducer, StreamConsumer};
use swarm_onboarding::qr::{Qr, QrPayload, QrTextType};
use swarm_onboarding::zenoh::device::DeviceOnboarding;
use swarm_onboarding::zenoh::installer::Installer;
use swarm_onboarding::{
    DeviceError, DeviceKeys, DeviceProfile, InstallerError, OnboardedDevice, OpNetFlags,
};

use x509_cert::Certificate;
use x509_cert::der::Decode;

use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::{PipeLink, PipeLinkReceive, PipeLinkSend, PipeRead, PipeWrite};
use zenoh_nano::network::Network;
use zenoh_nano::rng::{RandomSource, RngCore};
use zenoh_nano::scout::ZenohIdProto;
use zenoh_nano::session::{Session, SessionResources, SessionRunner};

use zenoh_traits::nano::ZNSession;
use zenoh_traits::{ErrorKind, ErrorType, Read};

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// These are generated using the following command line:
/// ```sh
/// openssl req -x509 -newkey ec:<(openssl ecparam -name prime256v1) -nodes -days 3000 -keyout onboarding_private.pem -out onboarding_cert.der -outform DER -subj "/C=DE/ST=/L=Munich/O=Peeriot Ltd/OU=Com/CN=DEVICE-ID:123456"
/// openssl ec -in onboarding_private.pem -outform DER -out onboarding_private.der
/// ```
///
/// To view the cert & key:
/// ```sh
/// openssl x509 -inform der -in onboarding_cert.der -text -noout
/// openssl ec -inform der -in onboarding_private.der -text -noout
/// ```
const CERTIFICATE: &[u8] = include_bytes!("onboarding_cert.der");
const PRIVATE_KEY: &[u8] = include_bytes!("onboarding_private.der");

fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_nanos()
        .init();

    let executor = EXECUTOR.init(Executor::new());
    executor.run(move |spawner: Spawner| {
        spawner.spawn(main_task(spawner).unwrap());
    });
}

macro_rules! mk_static {
    ($t:ty) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit();
        x
    }};
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write($val);
        x
    }};
}

/// Main task
#[embassy_executor::task]
async fn main_task(spawner: Spawner) {
    info!("Starting...");

    // Initialize links

    let pipe1 = mk_static!(PipeLink, PipeLink::new());
    let pipe2 = mk_static!(PipeLink, PipeLink::new());

    let (pipe1_read, pipe1_write) = pipe1.split();
    let (pipe2_read, pipe2_write) = pipe2.split();

    // Initialize networks

    let rng = RandomSource::new(mk_static!(LocalRng, LocalRng));

    let device_connect = Network::connect(
        PipeLinkReceive::new(PipeRead::new(pipe1_read), 100),
        PipeLinkSend::new(PipeWrite::new(pipe2_write), 100),
        Duration::from_secs(30),
        rng.clone(),
        ZenohIdProto::rand(),
    );

    let installer_accept = Network::accept(
        PipeLinkReceive::new(PipeRead::new(pipe2_read), 100),
        PipeLinkSend::new(PipeWrite::new(pipe1_write), 100),
        Duration::from_secs(30),
        rng,
        ZenohIdProto::rand(),
    );

    // Connect the networks together
    // Use `join` to run both futures concurrently and to await until both sides are connected
    let (device_network, installer_network) = join(device_connect, installer_accept).await;

    let device_network = device_network.unwrap();
    let installer_network = installer_network.unwrap();

    // Initialize sessions' resources

    let device_res = mk_static!(SessionResources, SessionResources::new());
    let device_pool = mk_static!(SubscriberPool, SubscriberPool::new());
    let installer_res = mk_static!(SessionResources, SessionResources::new());
    let installer_pool = mk_static!(SubscriberPool, SubscriberPool::new());

    // Create and run the sessions

    let (device_s, device_r) = Session::new(device_res, device_pool);
    let (installer_s, installer_r) = Session::new(installer_res, installer_pool);

    spawner.spawn(run_session(device_r, device_network).unwrap());
    spawner.spawn(run_session(installer_r, installer_network).unwrap());

    // Sample device credentials and profile

    // let device_keys = mk_static!(DeviceKeys, DeviceKeys::Insecure {
    //     device_id: b"example-device",
    // });

    let cert = mk_static!(Certificate, Certificate::from_der(CERTIFICATE).unwrap());
    let device_keys = mk_static!(
        DeviceKeys<'static>,
        DeviceKeys::PKI {
            pub_key: cert
                .tbs_certificate
                .subject_public_key_info
                .subject_public_key
                .as_bytes()
                .unwrap(),
            priv_key: PRIVATE_KEY
        }
    );

    let device_profile = DeviceProfile::new(OpNetFlags::all());

    let device_buf = mk_static!([u8; 3000], [0u8; 3000]);
    let creds = device_keys.creds();

    {
        let (device_profile_data, device_buf) = device_profile.serialize(device_buf).unwrap();

        let qr_payload = QrPayload::new(creds.pub_key().unwrap(), device_profile_data);
        let (qr_text, buf) = qr_payload.as_str(device_buf).unwrap();

        info!("QR-Text: {}", qr_text);

        let (tmp_buf, out_buf) = buf.split_at_mut(buf.len() / 2);

        let qr = Qr::compute(qr_text, tmp_buf, out_buf).unwrap();

        for line in qr.lines_range(QrTextType::Unicode, 4) {
            let (str, _) = qr
                .line_as_str(QrTextType::Unicode, 4, false, false, line, tmp_buf)
                .unwrap();

            info!("{}", str);
        }
    }

    let onboarded_device = mk_static!(
        OnboardedDevice<'static>,
        OnboardedDevice::new(device_keys.creds(), device_profile)
    );

    // Run the installer and device onboarding tasks

    let installer_buf = mk_static!([u8; 3000], [0u8; 3000]);

    spawner.spawn(device(ZNSession::new(device_s), device_keys, device_buf).unwrap());
    spawner.spawn(installer(ZNSession::new(installer_s), onboarded_device, installer_buf).unwrap());
}

/// Device task:
/// - Initiate the onboarding process by communicating out-of-band
///   the Device Credentials and Profile to the Installer;
/// - Then listen for the onboarding meta-data and bundle, download and process those.
#[embassy_executor::task]
async fn device(
    session: ZNSession<'static>,
    device_keys: &'static DeviceKeys<'static>,
    buf: &'static mut [u8],
) {
    info!("Running device...");

    let result: Result<(), DeviceError<ErrorKind, ErrorKind>> = async {
        let mut dbuf = [0; 1];
        let mut device = DeviceOnboarding::new(
            session,
            SliceProducer::new(CERTIFICATE),
            SliceConsumer::new(&mut dbuf),
        );

        device
            .onboard(
                device_keys,
                &mut rand_08::thread_rng(),
                &mut rand_08::thread_rng(),
                buf,
            )
            .await?;

        Ok(())
    }
    .await;

    result.unwrap();
}

/// Installer task:
/// - Get the Device Credentials and Profile out of band from the device;
/// - Then provide the onboarding meta-data and bundle and wait for the device to signal that
///   it has processed those and completed its onboarding.
#[embassy_executor::task]
async fn installer(
    session: ZNSession<'static>,
    device: &'static OnboardedDevice<'static>,
    buf: &'static mut [u8],
) {
    info!("Running installer...");

    let (dbuf, buf) = buf.split_at_mut(1000);

    /// A sample consumer that validates that the received device certificate
    /// does match the public key of the onboarded device.
    ///
    /// Other than that, nothing else is verified - i.e. the certificate signature and validity period
    struct CertConsumer<'a>(&'a mut [u8], &'a OnboardedDevice<'a>);

    impl ErrorType for CertConsumer<'_> {
        type Error = ErrorKind;
    }

    impl StreamConsumer for CertConsumer<'_> {
        async fn consume<R: Read, F: FnMut()>(
            &mut self,
            stream: R,
            progress_notify: F,
        ) -> Result<(), Self::Error> {
            let mut slc = SliceConsumer::new(self.0);

            slc.consume(stream, progress_notify).await?;

            let data = slc.data();

            info!("Consuming device certificate data of length {}", data.len());

            if !data.is_empty() {
                let cert = Certificate::from_der(data).map_err(|_| ErrorKind::InvalidData)?;
                let pub_key = cert
                    .tbs_certificate
                    .subject_public_key_info
                    .subject_public_key
                    .as_bytes()
                    .unwrap();

                let device_pub_key = self.1.creds().pub_key().unwrap();

                info!(
                    "Received device certificate with public key: {:x?}",
                    pub_key
                );
                info!("Expected device public key: {:x?}", device_pub_key);

                if pub_key != device_pub_key {
                    return Err(ErrorKind::InvalidData);
                }
            }

            Ok(())
        }
    }

    let result: Result<(), InstallerError<ErrorKind, ErrorKind>> = async {
        let mut installer = Installer::new(
            &session,
            CertConsumer(dbuf, device),
            SliceProducer::new(&[]),
        );

        installer
            .onboard(device, &mut rand_08::thread_rng(), buf)
            .await?;

        Ok(())
    }
    .await;

    result.unwrap();
}

/// Run the transport of a Zenoh session
#[embassy_executor::task(pool_size = 2)]
async fn run_session(
    mut runner: SessionRunner<'static>,
    network: Network<'static, PipeLinkReceive<'static>, PipeLinkSend<'static>>,
) {
    runner.run(network).await.unwrap()
}

struct LocalRng;

impl RngCore for LocalRng {
    fn next_u32(&mut self) -> u32 {
        rand::rng().next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        rand::rng().next_u64()
    }

    fn fill_bytes(&mut self, dst: &mut [u8]) {
        rand::rng().fill_bytes(dst);
    }
}
