//! This module contains the implementation of the device side of the Swarm Onboarding process.

use elliptic_curve::rand_core::CryptoRngCore;

use embedded_io_async::{Error, ErrorKind, Read, Write};

use crate::io::{ReadWrapper, SerdeObj, StreamConsumer, StreamProducer, WriteWrapper};
use crate::pipe::io::{RecvMessage, SendMessage};
use crate::utils::BufferOverflowError;
use crate::utils::io::read_all;
use crate::{DeviceError, DeviceKeys, InstallerMeta, OnboardingStatus};

/// The device side of the Swarm Onboarding process.
pub struct DeviceOnboarding<R, W, P, C> {
    read: R,
    write: W,
    att_producer: P,
    data_consumer: C,
}

impl<R, W, P, C> DeviceOnboarding<R, W, P, C>
where
    R: Read,
    W: Write,
    P: StreamProducer,
    C: StreamConsumer,
    P::Error: From<ErrorKind> + 'static,
    C::Error: From<ErrorKind> + 'static,
{
    /// Create a new `DeviceOnboarding` instance.
    ///
    /// # Arguments
    /// - `session`: The Zenoh session to use for the onboarding process.
    /// - `att_producer`: The attestation producer for producing the device attestation messages.
    /// - `data_consumer`: The data consumer for consuming the onboarding data messages.
    ///
    /// # Returns
    /// - A new `DeviceOnboarding` instance.
    pub const fn new(read: R, write: W, att_producer: P, data_consumer: C) -> Self {
        Self {
            read,
            write,
            att_producer,
            data_consumer,
        }
    }

    /// Run the device onboarding process.
    ///
    /// # Arguments
    /// - `device_keys`: The device keys used for authentication, attestation and decrypting the onboarding bundle.
    ///   The corresponding credentials (`DeviceCreds`)should be delivered to the installer out of band before starting the onboarding process.
    /// - `processor`: The processor used to process the onboarding bundle.
    /// - `read_factory`: The factory to use to read the delivered data bundle, which can be e.g. a TAR, a CIB, a TGZ etc.
    /// - `rng`: A cryptographic random number generator.
    /// - `buf`: A mutable byte slice that can be used for temporary storage during the onboarding process.
    ///
    /// # Returns
    /// - `Ok(())`: The onboarding process completed successfully.
    /// - `Err(e)`: An error occurred during the onboarding process.
    pub async fn onboard<RR>(
        &mut self,
        device_keys: &DeviceKeys<'_>,
        rng: &mut RR,
        buf: &mut [u8],
    ) -> Result<(), DeviceError<P::Error, C::Error>>
    where
        RR: CryptoRngCore,
    {
        let creds = device_keys.creds();
        let (device_id, buf) = creds.device_id(buf)?;

        info!("About to onboard device ID: {}", device_id);

        // Obtain the onboarding bundle meta-data first

        if buf.len() < InstallerMeta::MAX_BUF_SIZE {
            Err(BufferOverflowError)?;
        }

        let (meta_buf, buf) = buf.split_at_mut(InstallerMeta::MAX_BUF_SIZE);

        let meta = {
            info!("Waiting to receive onboarding meta-data...");

            let msg_read = RecvMessage::new(&mut self.read);

            let len = read_all(msg_read, meta_buf, Some(ErrorKind::OutOfMemory))
                .await
                .map_err(DeviceError::Io)?;

            let meta = InstallerMeta::deserialize(&meta_buf[..len])
                .map_err(|_| DeviceError::InvalidInstallerMeta)?;

            info!("Successfully received onboarding meta-data: {:?}", meta);

            meta
        };

        // Now establish a (potentially) secure channel using the derived channel key
        // From here onwards all communication is encrypted

        let channel_key = device_keys.derive_channel_key(&meta, buf)?;

        let (mut read_wrapper, buf) = channel_key.read_wrapper(buf);
        let (mut write_wrapper, buf) = channel_key.write_wrapper(rng, buf);

        // Send the attestation message

        {
            info!("Sending device attestation message...");

            let mut msg_write = SendMessage::new(&mut self.write);

            {
                let mut write_wrapper = write_wrapper.wrap(&mut msg_write);

                self.att_producer
                    .produce(&mut write_wrapper, || ())
                    .await
                    .map_err(DeviceError::Attestation)?;

                write_wrapper.flush().await.map_err(DeviceError::Io)?;
            }

            msg_write
                .close()
                .await
                .map_err(|e| DeviceError::Io(e.kind()))?;

            info!("Device attestation message sent successfully");
        }

        // Get the onboarding data

        {
            info!("Waiting to receive onboarding data...");

            let msg_read = read_wrapper.wrap(RecvMessage::new(&mut self.read));

            self.data_consumer
                .consume(
                    msg_read,
                    || (), /*For now, not interested in tracking progress by the consumer*/
                )
                .await
                .map_err(DeviceError::Data)?;

            info!("Onboarding data received successfully");
        }

        // Send info that the onboarding is complete

        {
            info!("Sending onboarding completion status...");

            let status = OnboardingStatus {
                done: true,
                status: "Completed",
            };

            let (status_payload, _) = status.serialize(buf)?;

            let mut msg_write = SendMessage::new(&mut self.write);

            {
                let mut write_wrapper = write_wrapper.wrap(&mut msg_write);

                write_wrapper
                    .write_all(status_payload)
                    .await
                    .map_err(DeviceError::Io)?;

                write_wrapper.flush().await.map_err(DeviceError::Io)?;
            }

            msg_write
                .close()
                .await
                .map_err(|e| DeviceError::Io(e.kind()))?;
        }

        info!("Device onboarding completed successfully");

        Ok(())
    }
}
