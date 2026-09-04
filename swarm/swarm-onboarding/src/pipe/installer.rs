//! This module contains the implementation of the installer side of the Swarm Onboarding process over a `Read` + `Write` pipe.

use elliptic_curve::rand_core::CryptoRngCore;

use embedded_io_async::{Error, ErrorKind, Read, Write};

use crate::io::{ReadWrapper, SerdeObj, StreamConsumer, StreamProducer, WriteWrapper};
use crate::pipe::io::{RecvMessage, SendMessage};
use crate::utils::BufferOverflowError;
use crate::utils::io::read_all;
use crate::{InstallerError, OnboardedDevice, OnboardingStatus};

/// The installer side of the Swarm Onboarding process.
pub struct Installer<R, W, C, P> {
    read: R,
    write: W,
    att_consumer: C,
    data_producer: P,
}

impl<R, W, C, P> Installer<R, W, C, P>
where
    R: Read,
    W: Write,
    C: StreamConsumer,
    C::Error: From<zenoh_traits::ErrorKind>,
    P: StreamProducer,
    P::Error: From<zenoh_traits::ErrorKind>,
{
    /// Create a new `Installer` instance.
    ///
    /// # Arguments
    /// - `read`: The read pipe from the device to the installer.
    /// - `write`: The write pipe from the installer to the device.
    /// - `att_consumer`: The attestation consumer for consuming the device attestation messages.
    /// - `data_producer`: The data producer for producing the onboarding data messages.
    ///
    /// # Returns
    /// - A new `Installer` instance.
    pub const fn new(read: R, write: W, att_consumer: C, data_producer: P) -> Self {
        Self {
            read,
            write,
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
    pub async fn onboard<'a, RR>(
        &mut self,
        device: &'a OnboardedDevice<'a>,
        rng: &'a mut RR,
        buf: &'a mut [u8],
    ) -> Result<(), InstallerError<C::Error, P::Error>>
    where
        RR: CryptoRngCore,
    {
        let (device_id, buf) = device.device_id(buf)?;

        info!(
            "About to onboard device ID: {}, profile: {}",
            device_id, device.profile
        );

        // Derive the symmetric key and prepare the meta payload

        let (channel_key, meta, buf) = device.derive_channel_key(rng, buf)?;

        info!("Bundle meta: {:?}", meta);

        // Publish the metadata to the device (in plaintext)

        {
            info!("Sending onboarding meta to device");

            let mut msg_write = SendMessage::new(&mut self.write);

            let (meta_payload, _) = meta.serialize(buf)?;

            info!("Onboarding meta payload length: {}", meta_payload.len());

            msg_write
                .write_all(meta_payload)
                .await
                .map_err(|e| InstallerError::Io(e.kind()))?;

            msg_write
                .close()
                .await
                .map_err(|e| InstallerError::Io(e.kind()))?;

            info!("Sent onboarding meta to device");
        }

        // From here onwards all communication is encrypted

        let (mut read_wrapper, buf) = channel_key.read_wrapper(buf);
        let (mut write_wrapper, buf) = channel_key.write_wrapper(rng, buf);

        // Wait for the device to send its credentials

        {
            info!("Waiting for device attestation message");

            let msg_read = read_wrapper.wrap(RecvMessage::new(&mut self.read));

            self.att_consumer
                .consume(msg_read, || ())
                .await
                .map_err(InstallerError::Attestation)?;

            info!("Device attestation completed successfully");
        }

        // Publish the onboarding data bundle to the device

        {
            info!("Sending onboarding data to device");

            let mut msg_write = SendMessage::new(&mut self.write);

            {
                let mut write_wrapper = write_wrapper.wrap(&mut msg_write);

                self.data_producer
                    .produce(&mut write_wrapper, || ())
                    .await
                    .map_err(InstallerError::Data)?;

                write_wrapper.flush().await.map_err(InstallerError::Io)?;
            }

            msg_write
                .close()
                .await
                .map_err(|e| InstallerError::Io(e.kind()))?;

            info!("Onboarding data sent to device successfully");
        }

        // Wait for the device to complete the onboarding process

        if buf.len() < OnboardingStatus::MAX_BUF_SIZE {
            Err(BufferOverflowError)?;
        }

        loop {
            info!("Waiting for device onboarding status update...");

            let msg_read = read_wrapper.wrap(RecvMessage::new(&mut self.read));

            let len = read_all(msg_read, buf, Some(ErrorKind::OutOfMemory))
                .await
                .map_err(InstallerError::Io)?;

            let status = OnboardingStatus::deserialize(&buf[..len])
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
