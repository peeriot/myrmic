//! This module contains the implementation of the device side of the Swarm Onboarding process.

use core::pin::pin;

use elliptic_curve::rand_core::CryptoRngCore;

use embassy_futures::select::select3;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::signal::Signal;

use embedded_io_async::ErrorKind;

use futures_util::TryFutureExt;

use zenoh_traits::{Sender, Session};

use crate::fmt::Bytes;
use crate::io::{
    SerdeObj, SliceConsumer, SliceProducer, StreamConsumer, StreamProducer, WrappingConsumer,
    WrappingProducer, WriteWrapper,
};
use crate::utils::BufferOverflowError;
use crate::utils::future::Coalesce as _;
use crate::zenoh::QUERY_RETRY_TIMEOUT;
use crate::{DeviceError, DeviceKeys, InstallerMeta, OnboardingStatus};

use super::io::{get, send, set};

/// The device side of the Swarm Onboarding process.
pub struct DeviceOnboarding<T, P, C> {
    session: T,
    att_producer: P,
    data_consumer: C,
}

impl<T, P, C> DeviceOnboarding<T, P, C>
where
    T: Session,
    P: StreamProducer,
    C: StreamConsumer,
    P::Error: From<ErrorKind>,
    C::Error: From<ErrorKind>,
{
    /// Create a new `DeviceOnboarding` instance.
    ///
    /// # Arguments
    /// - `session`: The Zenoh session to use for the onboarding process.
    /// - `att_producer`: The producer for the device attestation messages.
    /// - `data_consumer`: The consumer for the onboarding data messages.
    ///
    /// # Returns
    /// - A new `DeviceOnboarding` instance.
    pub const fn new(session: T, att_producer: P, data_consumer: C) -> Self {
        Self {
            session,
            att_producer,
            data_consumer,
        }
    }

    /// Run the device onboarding process.
    ///
    /// # Arguments
    /// - `device_keys`: The device keys used for authentication, attestation and decrypting the onboarding bundle.
    ///   The corresponding credentials (`DeviceCreds`)should be delivered to the installer out of band before starting the onboarding process.
    /// - `rng1`: A cryptographic random number generator.
    /// - `rng2`: A second cryptographic random number generator.
    /// - `buf`: A mutable byte slice that can be used for temporary storage during the onboarding process.
    ///
    /// # Returns
    /// - `Ok(())`: The onboarding process completed successfully.
    /// - `Err(e)`: An error occurred during the onboarding process.
    pub async fn onboard<R>(
        &mut self,
        device_keys: &DeviceKeys<'_>,
        rng1: &mut R,
        rng2: &mut R,
        buf: &mut [u8],
    ) -> Result<(), DeviceError<P::Error, C::Error>>
    where
        R: CryptoRngCore,
    {
        // Prepare topics and buffers

        let creds = device_keys.creds();
        let (device_id, buf) = creds.device_id(buf)?;

        info!("About to onboard device ID: {}", device_id);

        let (meta_topic, buf) = write_dbuf!(buf, "@onboarding/@v1/@-meta/@{}", device_id)?;
        let (att_topic, buf) = write_dbuf!(buf, "@onboarding/@v1/@-att/@{}", device_id)?;
        let (data_topic, buf) = write_dbuf!(buf, "@onboarding/@v1/@-data/@{}", device_id)?;
        let (status_topic, buf) = write_dbuf!(buf, "@onboarding/@v1/@-status/@{}", device_id)?;

        // Request onboarding & obtain the onboarding bundle meta-data first

        let (meta, buf) = self.receive_meta(meta_topic, buf).await?;

        // Now establish a (potentially) secure channel using the derived channel key

        let channel_key = device_keys.derive_channel_key(&meta, buf)?;

        let (mut read_wrapper, buf) = channel_key.read_wrapper(buf);
        let (mut write_wrapper, buf) = channel_key.write_wrapper(rng1, buf);
        let (mut write_wrapper_status, buf) = channel_key.write_wrapper(rng2, buf);

        // Prepare all remaining tasks which run in parallel during the rest of the onboarding process
        // Using `pin!` is completely optional, but it does help for the `run` future to have a smaller
        // memory size in that it avoids expensive moves of the sub-futures into the `select4` call

        if buf.len() < OnboardingStatus::MAX_BUF_SIZE {
            Err(BufferOverflowError)?;
        }

        let mut status_sender = Self::status_sender(&self.session, status_topic).await?;
        let progress = Signal::<NoopRawMutex, ()>::new();

        {
            let mut send_att = pin!(
                set(
                    &self.session,
                    att_topic,
                    WrappingProducer::new(&mut self.att_producer, &mut write_wrapper),
                    || (),
                )
                .map_err(DeviceError::Attestation)
            );

            let mut receive_data = pin!(
                get(
                    &self.session,
                    data_topic,
                    QUERY_RETRY_TIMEOUT,
                    WrappingConsumer::new(&mut self.data_consumer, &mut read_wrapper),
                    || (),
                )
                .map_err(DeviceError::Data)
            );

            let mut send_progress = pin!(Self::send_progress(
                &mut status_sender,
                &mut write_wrapper_status,
                &progress,
                buf,
            ));

            select3(&mut send_att, &mut receive_data, &mut send_progress)
                .coalesce()
                .await?;
        }

        // Send the final "done" status, now that onboarding is complete

        let status = OnboardingStatus {
            done: true,
            status: "Completed",
        };

        Self::send_status(status_sender, write_wrapper_status, &status, buf).await?;

        info!("Device onboarding completed successfully");

        Ok(())
    }

    /// Receive the onboarding bundle metadata.
    ///
    /// # Arguments
    /// - `topic`: The topic to receive the metadata from.
    /// - `buf`: A mutable byte slice that can be used for temporary storage.
    ///
    /// # Returns
    /// - `Ok((meta, buf))`: The onboarding bundle metadata and the remaining buffer space.
    /// - `Err(e)`: An error occurred while receiving or parsing the metadata.
    async fn receive_meta<'b>(
        &self,
        topic: &str,
        buf: &'b mut [u8],
    ) -> Result<(InstallerMeta<'b>, &'b mut [u8]), DeviceError<P::Error, C::Error>> {
        info!("Waiting for onboarding meta-data on topic: {}...", topic);

        let size = {
            let mut consumer = SliceConsumer::new(buf);

            get(
                &self.session,
                topic,
                QUERY_RETRY_TIMEOUT,
                &mut consumer,
                || (),
            )
            .await?;

            consumer.size()
        };

        let (data, buf) = buf.split_at_mut(unwrap!(size));

        let meta =
            InstallerMeta::deserialize(data).map_err(|_| DeviceError::InvalidInstallerMeta)?;

        info!(
            "Successfully received onboarding meta-data: {:?} on topic {}",
            meta, topic
        );

        Ok((meta, buf))
    }

    /// Send periodic progress updates while processing the onboarding bundle.
    ///
    /// # Arguments
    /// - `sender`: The sender used to send the progress updates.
    /// - `write_wrapper`: A write wrapper for potentially encrypting the progress updates.
    /// - `progress`: A signal that is triggered to indicate progress in processing the onboarding bundle.
    /// - `buf`: A mutable byte slice that can be used for temporary storage.
    ///
    /// # Returns
    /// - `Err(e)`: An error occurred while sending the progress updates.
    async fn send_progress<S, W>(
        mut sender: S,
        mut write_wrapper: W,
        progress: &Signal<impl RawMutex, ()>,
        buf: &mut [u8],
    ) -> Result<(), DeviceError<P::Error, C::Error>>
    where
        S: Sender<Error = T::Error>,
        W: WriteWrapper,
    {
        loop {
            progress.wait().await;

            let status = OnboardingStatus {
                done: false,
                status: "In progress",
            };

            Self::send_status(&mut sender, &mut write_wrapper, &status, buf).await?;
        }
    }

    /// Create a sender for sending onboarding status updates.
    ///
    /// # Arguments
    /// - `session`: The Zenoh session to use for communication.
    /// - `topic`: The topic to send the status updates to.
    ///
    /// # Returns
    /// - `Ok(sender)`: The sender for sending status updates.
    /// - `Err(e)`: An error occurred while creating the sender.
    async fn status_sender<'b>(
        session: &'b T,
        topic: &'b str,
    ) -> Result<impl Sender<Error = T::Error> + 'b, DeviceError<P::Error, C::Error>> {
        session.publish(topic).await.map_err(DeviceError::io)
    }

    /// Send an onboarding status update.
    ///
    /// # Arguments
    /// - `sender`: The sender used to send the status update.
    /// - `write_wrapper`: A write wrapper for potentially encrypting the status update.
    /// - `status`: The status update to send.
    /// - `buf`: A mutable byte slice that can be used for temporary storage.
    ///
    /// # Returns
    /// - `Ok(())`: The status update was sent successfully.
    /// - `Err(e)`: An error occurred while sending the status update.
    async fn send_status<S: Sender<Error = T::Error>, W>(
        sender: S,
        write_wrapper: W,
        status: &OnboardingStatus<'_>,
        buf: &mut [u8],
    ) -> Result<(), DeviceError<P::Error, C::Error>>
    where
        W: WriteWrapper,
    {
        info!("About to send device status: {:?}", status);

        let (status_payload, _) = status.serialize(buf)?;

        debug!(
            "Device status payload: {:?}, len: {}B",
            Bytes(status_payload),
            status_payload.len()
        );

        send(
            sender,
            "",
            WrappingProducer::new(SliceProducer::new(status_payload), write_wrapper),
            || (),
        )
        .await?;

        info!("Successfully sent device status.");

        Ok(())
    }
}

macro_rules! write_dbuf {
    ($dst:expr, $($arg:tt)*) => {{
        use crate::utils::slicebuf::write_buf;

        write_buf!($dst, $($arg)*)
    }};
}

use write_dbuf;
