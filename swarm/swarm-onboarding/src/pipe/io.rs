use embedded_io_async::{ErrorType, Read, Write};

/// A type for sending messages over a `Write` pipe
/// by encoding it as described in the `pipe` module.
///
/// NOTE:
/// The `Write::write` implementation this type provides is NOT cancel-safe.
pub struct SendMessage<T> {
    write: T,
    eof: bool,
}

impl<T> SendMessage<T> {
    /// Create a new `SendMessage` instance.
    pub const fn new(write: T) -> Self {
        Self { write, eof: false }
    }
}

impl<T> SendMessage<T>
where
    T: Write,
{
    /// Close the message stream by writing the EOF byte.
    ///
    /// NOTE:
    /// This method is NOT cancel-safe.
    pub async fn close(&mut self) -> Result<(), T::Error> {
        if !self.eof {
            self.write.write_all(&[0]).await?;
            self.eof = true;

            trace!("Wrote EOF byte");
        }

        Ok(())
    }
}

impl<T> Drop for SendMessage<T> {
    fn drop(&mut self) {
        if !self.eof {
            warn!("SendMessage dropped without being closed properly");
        }
    }
}

impl<T> ErrorType for SendMessage<T>
where
    T: Write,
{
    type Error = T::Error;
}

impl<T> Write for SendMessage<T>
where
    T: Write,
{
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if data.is_empty() || self.eof {
            return Ok(0);
        }

        for chunk in data.chunks(u8::MAX as usize) {
            let len = chunk.len() as u8;
            self.write.write_all(&[len]).await?;
            self.write.write_all(chunk).await?;

            trace!("Wrote chunk of {} bytes", len);
        }

        Ok(data.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.write.flush().await
    }
}

/// A type for receiving messages over a `Read` pipe
/// by decoding it as described in the `pipe` module.
pub struct RecvMessage<T> {
    read: T,
    remaining_len: u8,
    eof: bool,
}

impl<T> RecvMessage<T> {
    /// Create a new `RecvMessage` instance.
    pub const fn new(read: T) -> Self {
        Self {
            read,
            remaining_len: 0,
            eof: false,
        }
    }
}

impl<T> ErrorType for RecvMessage<T>
where
    T: Read,
{
    type Error = T::Error;
}

impl<T> Read for RecvMessage<T>
where
    T: Read,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() || self.eof {
            return Ok(0);
        }

        if self.remaining_len == 0 {
            let mut remaining_len_buf = [0u8];
            let len = self.read.read(&mut remaining_len_buf).await?;
            self.remaining_len = remaining_len_buf[0];

            if len == 0 || self.remaining_len == 0 {
                trace!("EOF reached");
                self.eof = true;
                return Ok(0);
            }

            trace!("Next chunk: {} bytes remaining", self.remaining_len);
        }

        let max_len = buf.len().min(self.remaining_len as usize);
        let len = self.read.read(&mut buf[..max_len]).await?;
        self.remaining_len -= len as u8;

        trace!(
            "Read {} bytes, {} bytes remaining in chunk",
            len, self.remaining_len
        );

        Ok(len)
    }
}

#[cfg(test)]
mod test {
    use embedded_io_async::ErrorKind;

    use crate::utils::{io::read_all, slicebuf::SliceBuf};

    use super::*;

    fn read_msg<'a>(buf: &'a mut [u8], data: &[u8]) -> &'a [u8] {
        embassy_futures::block_on(async move {
            let mut read = data;

            let read_len = read_all(
                RecvMessage::new(&mut read),
                buf,
                Some(ErrorKind::OutOfMemory),
            )
            .await
            .unwrap();

            &buf[..read_len]
        })
    }

    fn write_msg<'a>(buf: &'a mut [u8], data: &[u8]) -> &'a [u8] {
        embassy_futures::block_on(async move {
            let len = {
                let mut write = SliceBuf::new(buf);

                {
                    let mut send_msg = SendMessage::new(&mut write);

                    send_msg.write_all(data).await.unwrap();
                    send_msg.close().await.unwrap();
                }

                write.len()
            };

            &buf[..len]
        })
    }

    #[test]
    fn test_read() {
        let mut read_buf = [0u8; 1024];

        assert_eq!(read_msg(&mut read_buf, &[]), b"");
        assert_eq!(read_msg(&mut read_buf, &[0]), b"");
        assert_eq!(read_msg(&mut read_buf, &[1, 2, 0]), &[2]);
        assert_eq!(
            read_msg(&mut read_buf, &[1, 2, 1, 3, 5, 4, 4, 4, 4, 4, 0]),
            &[2, 3, 4, 4, 4, 4, 4]
        );
        assert_eq!(
            read_msg(&mut read_buf, &[1, 2, 1, 3, 5, 4, 4, 4, 4, 4, 0, 1, 0]),
            &[2, 3, 4, 4, 4, 4, 4]
        );
    }

    #[test]
    fn test_write() {
        let mut write_buf = [0u8; 1024];

        assert_eq!(write_msg(&mut write_buf, &[]), &[0]);
        assert_eq!(write_msg(&mut write_buf, &[5]), &[1, 5, 0]);
        assert_eq!(
            write_msg(&mut write_buf, &[5, 2, 2, 2]),
            &[4, 5, 2, 2, 2, 0]
        );
    }

    #[test]
    fn test_send_recv() {
        let data = b"Hello, world! This is a test message for SendMessage and RecvMessage.";

        let mut read_buf = [0u8; 1024];
        let mut write_buf = [0u8; 1024];

        let written = write_msg(&mut write_buf, data);
        let read = read_msg(&mut read_buf, written);

        assert_eq!(read, data);
    }
}
