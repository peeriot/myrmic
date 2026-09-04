//! This module contains the implementation of the installer side of the Swarm Onboarding process.

use core::pin::pin;

use elliptic_curve::rand_core::CryptoRngCore;

use embassy_futures::select::select;

use futures_util::{FutureExt, TryFutureExt};

use crate::io::{
    ReadWrapper, SerdeObj, SliceConsumer, SliceProducer, StreamConsumer, StreamProducer,
    WrappingConsumer, WrappingProducer,
};
use crate::utils::BufferOverflowError;
use crate::utils::future::Coalesce as _;
use crate::zenoh::QUERY_RETRY_TIMEOUT;
use crate::{InstallerError, OnboardedDevice, OnboardingStatus};

use super::io::{get, receive, set};

/// The installer side of the Swarm Onboarding process.
pub struct Installer<T, C, P> {
    session: T,
    att_consumer: C,
    data_producer: P,
}

impl<T, C, P> Installer<T, C, P>
where
    T: zenoh_traits::Session,
    C: StreamConsumer,
    C::Error: From<zenoh_traits::ErrorKind>,
    P: StreamProducer,
    P::Error: From<zenoh_traits::ErrorKind>,
{
    /// Create a new `Installer` instance.
    ///
    /// # Arguments
    /// - `session`: A Zenoh session to use for communication with the device.
    /// - `att_consumer`: A consumer for the device attestation messages.
    /// - `data_producer`: A producer for the onboarding data messages.
    ///
    /// # Returns
    /// - A new `Installer` instance.
    pub const fn new(session: T, att_consumer: C, data_producer: P) -> Self {
        Self {
            session,
            att_consumer,
            data_producer,
        }
    }

    /// Run the onboarding process for a given device.
    ///
    /// # Arguments
    /// - `device`: The device to onboard.
    ///   The device information must have been obtained out-of-band before calling this method.
    /// - `rng`: A cryptographic random number generator.
    /// - `buf`: A buffer for temporary data storage.
    ///
    /// # Returns
    /// - `Ok(())`: The onboarding process was successful.
    /// - `Err(InstallerError)`: An error occurred during the onboarding process.
    pub async fn onboard<'a, R>(
        &mut self,
        device: &'a OnboardedDevice<'a>,
        rng: &'a mut R,
        buf: &'a mut [u8],
    ) -> Result<(), InstallerError<C::Error, P::Error>>
    where
        R: CryptoRngCore,
    {
        let (device_id, buf) = device.device_id(buf)?;

        info!(
            "About to onboard device ID: {}, profile: {}",
            device_id, device.profile
        );

        // Prepare topics and buffers

        let (meta_topic, buf) = write_ibuf!(buf, "@onboarding/@v1/@-meta/@{}", device_id)?;
        let (att_topic, buf) = write_ibuf!(buf, "@onboarding/@v1/@-att/@{}", device_id)?;
        let (data_topic, buf) = write_ibuf!(buf, "@onboarding/@v1/@-data/@{}", device_id)?;
        let (status_topic, buf) = write_ibuf!(buf, "@onboarding/@v1/@-status/@{}", device_id)?;

        // Derive the symmetric key and prepare the meta payload

        let (channel_key, meta, buf) = device.derive_channel_key(rng, buf)?;

        info!("Bundle meta: {:?}", meta);

        // Now run the first part of the onboarding process:
        // - Publish the metadata to the device
        // - Wait for the device to send its credentials
        //
        // Using `pin!` is completely optional, but it does help for the `run` future to have a smaller
        // memory size in that it avoids expensive moves of the sub-futures into the `select4` call

        let (meta_payload, buf) = meta.serialize(buf)?;

        let (mut read_wrapper, buf) = channel_key.read_wrapper(buf);
        let (write_wrapper, buf) = channel_key.write_wrapper(rng, buf);

        {
            let mut publish_meta = pin!(
                set(
                    &self.session,
                    meta_topic,
                    SliceProducer::new(meta_payload),
                    || (),
                )
                .map_err(InstallerError::Io)
            );

            let mut receive_att = pin!(
                get(
                    &self.session,
                    att_topic,
                    QUERY_RETRY_TIMEOUT,
                    WrappingConsumer::new(&mut self.att_consumer, &mut read_wrapper),
                    || (),
                )
                .map_err(InstallerError::Attestation)
            );

            select(&mut publish_meta, &mut receive_att)
                .coalesce()
                .await?;
        }

        // Now run the second part of the onboarding process:
        // - Publish the onboarding data bundle to the device
        // - Wait for the device to complete the onboarding process

        if buf.len() < OnboardingStatus::MAX_BUF_SIZE {
            Err(BufferOverflowError)?;
        }

        let mut publish_data = pin!(
            set(
                &self.session,
                data_topic,
                WrappingProducer::new(&mut self.data_producer, write_wrapper),
                || (),
            )
            .map(|r| r.map_err(InstallerError::Data))
        );

        let mut wait_device = pin!(Self::wait_device(
            &self.session,
            status_topic,
            device,
            read_wrapper,
            buf
        ));

        select(&mut publish_data, &mut wait_device).coalesce().await
    }

    /// Wait for the device to complete the onboarding process by monitoring its status updates.
    ///
    /// # Arguments
    /// - `session`: The Zenoh session to use for communication with the device.
    /// - `topic`: The topic to subscribe to for receiving device status updates.
    /// - `device`: The device being onboarded.
    /// - `read_wrapper`: A factory to use for wrapping the read stream returned by the subscriber.
    /// - `buf`: A buffer for temporary data storage.
    async fn wait_device<R>(
        session: &T,
        topic: &str,
        device: &OnboardedDevice<'_>,
        mut read_wrapper: R,
        buf: &mut [u8],
    ) -> Result<(), InstallerError<C::Error, P::Error>>
    where
        R: ReadWrapper,
    {
        info!(
            "Waiting for device {} to complete onboarding...",
            device.device_id(buf)?.0
        );

        let mut subscriber = session.subscribe(topic).await.map_err(InstallerError::io)?;

        loop {
            let mut consumer = SliceConsumer::new(buf);

            receive(
                &mut subscriber,
                WrappingConsumer::new(&mut consumer, &mut read_wrapper),
                || (),
            )
            .await?;

            let data = consumer.data();

            let status = OnboardingStatus::deserialize(data)
                .map_err(|_| InstallerError::InvalidDeviceStatus)?;

            info!("Device status: {:?}", status);

            if status.done {
                info!("Device onboarding completed successfully");
                break;
            }
        }

        Ok(())
    }
}

macro_rules! write_ibuf {
    ($dst:expr, $($arg:tt)*) => {{
        use crate::utils::slicebuf::write_buf;

        write_buf!($dst, $($arg)*)
    }};
}

use write_ibuf;
