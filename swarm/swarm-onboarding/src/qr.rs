//! QR Code generation and rendering

use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::io::SerdeObjError;
use crate::utils::BufferOverflowError;
use crate::utils::base38::{decode, encode};
use crate::utils::slicebuf::SliceBuf;

/// QR Payload
pub struct QrPayload<'a> {
    /// Device credentials (usually, the public key of the device, where NistP256 EC is used)
    pub device_creds: &'a [u8],
    /// Device profile (optional)
    pub device_profile: &'a [u8],
}

impl<'a> QrPayload<'a> {
    /// Create a new QR payload
    ///
    /// # Arguments
    /// - `device_creds` - Device credentials (usually, the public key of the device, where NistP256 EC is used)
    /// - `device_profile` - Device profile (optional)
    pub const fn new(device_creds: &'a [u8], device_profile: &'a [u8]) -> Self {
        Self {
            device_creds,
            device_profile,
        }
    }

    /// Decode the QR payload from a QR string
    ///
    /// # Arguments
    /// - `qr_str` - An encoded QR string to decode, in the format emitted by `as_chars`
    ///   The string should be in the format `<creds>:<profile>`, where `:profile` is optional
    ///   and where each part is base38-encoded
    /// - `buf` - Buffer to store the decoded data
    ///
    /// # Returns
    /// - On success, returns a tuple containing the decoded QR payload and the remaining buffer
    /// - On failure, returns an error
    pub fn decode(qr_str: &str, buf: &'a mut [u8]) -> Result<(Self, &'a mut [u8]), SerdeObjError> {
        let (creds_str, profile_str) = qr_str.split_once(':').unwrap_or((qr_str, ""));

        let mut buf = SliceBuf::new(buf);
        for byte in decode(creds_str) {
            let byte = byte.map_err(|_| SerdeObjError::BufferOverflow)?;

            buf.extend(core::iter::once(byte))
                .map_err(|_| SerdeObjError::BufferOverflow)?;
        }

        let (device_creds, buf) = buf.split();

        let mut buf = SliceBuf::new(buf);

        for byte in decode(profile_str) {
            let byte = byte.map_err(|_| SerdeObjError::BufferOverflow)?;

            buf.extend(core::iter::once(byte))
                .map_err(|_| SerdeObjError::BufferOverflow)?;
        }

        let (device_profile, buf) = buf.split();

        Ok((
            Self {
                device_creds,
                device_profile,
            },
            buf,
        ))
    }

    /// Get an iterator over the characters of the QR payload
    pub fn as_chars(&self) -> impl Iterator<Item = char> + '_ {
        encode(self.device_creds).chain(
            (!self.device_profile.is_empty())
                .then_some(self.device_profile)
                .into_iter()
                .flat_map(|p| core::iter::once(':').chain(encode(p))),
        )
    }

    /// Encode the QR payload as a string into the provided buffer
    ///
    /// # Arguments
    /// - `buf` - Output buffer for the rendered string
    ///
    /// # Returns
    /// - On success, returns a tuple containing the rendered string and the remaining buffer
    /// - On failure, returns an error
    pub fn as_str<'b>(
        &self,
        buf: &'b mut [u8],
    ) -> Result<(&'b str, &'b mut [u8]), BufferOverflowError> {
        let mut buf = SliceBuf::new(buf);
        buf.extend(self.as_chars().map(|c| c as u8))?;

        let (str, buf) = unwrap!(buf.split_str().map_err(|_| ()));

        Ok((str, buf))
    }
}

/// QR Code text type
///
/// Used when emitting the QR code in different text formats
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum QrTextType {
    /// Pure ASCII text
    /// Compatible with all consoles
    Ascii,
    /// ANSI
    Ansi,
    /// Unicode
    Unicode,
}

/// QR Code representation
pub struct Qr<'a>(QrCode<'a>);

impl<'a> Qr<'a> {
    /// Create a new QR code from the given text
    ///
    /// # Arguments
    /// - `text` - Text to encode in the QR code
    /// - `tmp_buf` - Temporary buffer for QR code generation
    /// - `out_buf` - Output buffer for the QR code
    ///
    /// # Returns
    /// - On success, returns the generated QR code
    /// - On failure, returns an error
    pub fn compute(
        text: &str,
        tmp_buf: &mut [u8],
        out_buf: &'a mut [u8],
    ) -> Result<Self, BufferOverflowError> {
        const ECC: QrCodeEcc = QrCodeEcc::Medium;
        const MIN_VERSION: u8 = 1;
        const MAX_VERSION: u8 = 20;

        let qr = QrCode::encode_text(
            text,
            tmp_buf,
            out_buf,
            ECC,
            Version::new(MIN_VERSION),
            Version::new(MAX_VERSION),
            None,
            false,
        )
        .map_err(|_| BufferOverflowError)?;

        Ok(Self(qr))
    }

    /// Get the size of the QR code
    pub fn size(&self) -> u32 {
        self.0.size() as _
    }

    /// Get the module value at the given coordinates
    pub fn get_module(&self, x: i32, y: i32) -> bool {
        self.0.get_module(x, y)
    }

    /// Encode the QR as a string into the provided buffer
    ///
    /// # Arguments
    /// - `text_type` - Type of text to return (ASCII, ANSI, Unicode)
    /// - `border` - Border size
    /// - `invert` - Whether to invert the colors (black on a white background)
    /// - `out_buf` - Output buffer for the rendered string
    ///
    /// # Returns
    /// - On success, returns a tuple containing the rendered string and the remaining buffer
    /// - On failure, returns an error
    pub fn as_str<'b>(
        &self,
        text_type: QrTextType,
        border: u8,
        invert: bool,
        out_buf: &'b mut [u8],
    ) -> Result<(&'b str, &'b mut [u8]), BufferOverflowError> {
        let mut offset = 0;

        for c in self.emit_chars(text_type, border, invert) {
            let mut dst = [0; 4];
            let bytes = c.encode_utf8(&mut dst).as_bytes();

            if offset + bytes.len() > out_buf.len() {
                return Err(BufferOverflowError)?;
            } else {
                out_buf[offset..offset + bytes.len()].copy_from_slice(bytes);
                offset += bytes.len();
            }
        }

        let (str_buf, remaining_buf) = out_buf.split_at_mut(offset);

        // Can't fail as `emit_chars` generates a valid UTF-8 string
        let str = unwrap!(core::str::from_utf8(str_buf).map_err(|_| ()));

        Ok((str, remaining_buf))
    }

    /// Encode a single line of the QR as a string into the provided buffer
    ///
    /// # Arguments
    /// - `text_type` - Type of text to return (ASCII, ANSI, Unicode)
    /// - `border` - Border size
    /// - `invert` - Whether to invert the colors (black on a white background)
    /// - `nl` - Whether to add a newline at the end of the line
    /// - `y` - Y coordinate of the line to render
    /// - `out_buf` - Output buffer for the rendered string
    ///
    /// # Returns
    /// - On success, returns a tuple containing the rendered string and the remaining buffer
    /// - On failure, returns an error
    pub fn line_as_str<'b>(
        &self,
        text_type: QrTextType,
        border: u8,
        invert: bool,
        nl: bool,
        y: i32,
        out_buf: &'b mut [u8],
    ) -> Result<(&'b str, &'b mut [u8]), BufferOverflowError> {
        let mut offset = 0;

        for c in self.emit_line_chars(text_type, border, invert, nl, y) {
            let mut dst = [0; 4];
            let bytes = c.encode_utf8(&mut dst).as_bytes();

            if offset + bytes.len() > out_buf.len() {
                return Err(BufferOverflowError)?;
            } else {
                out_buf[offset..offset + bytes.len()].copy_from_slice(bytes);
                offset += bytes.len();
            }
        }

        let (str_buf, remaining_buf) = out_buf.split_at_mut(offset);

        // Can't fail as `emit_chars` generates a valid UTF-8 string
        let str = unwrap!(core::str::from_utf8(str_buf).map_err(|_| ()));

        Ok((str, remaining_buf))
    }

    /// Get an iterator over the indexes of the lines of the QR code including borders
    ///
    /// # Arguments
    /// - `text_type` - Type of text to return (ASCII, ANSI, Unicode)
    /// - `border` - Border size
    pub fn lines_range(
        &self,
        text_type: QrTextType,
        border: u8,
    ) -> impl Iterator<Item = i32> + '_ + 'a {
        let iborder: i32 = border as _;

        (-iborder..self.size() as i32 + iborder)
            .filter(move |y| !matches!(text_type, QrTextType::Unicode) || (*y - -iborder) % 2 == 0)
    }

    /// Get an iterator over the characters of the rendered QR code
    ///
    /// # Arguments
    /// - `text_type` - Type of text to return (ASCII, ANSI, Unicode)
    /// - `border` - Border size
    /// - `invert` - Whether to invert the colors (black on a white background)
    ///
    /// # Returns
    /// - An iterator over the characters of the rendered QR code
    pub fn emit_chars(
        &self,
        text_type: QrTextType,
        border: u8,
        invert: bool,
    ) -> impl Iterator<Item = char> + use<'_, 'a> {
        self.lines_range(text_type, border)
            .flat_map(move |y| self.emit_line_chars(text_type, border, invert, true, y))
    }

    /// Get an iterator over the characters of a single line of the rendered QR code
    ///
    /// # Arguments
    /// - `text_type` - Type of text to return (ASCII, ANSI, Unicode)
    /// - `border` - Border size
    /// - `invert` - Whether to invert the colors (black on a white background)
    /// - `nl` - Whether to add a newline at the end of the line
    /// - `y` - Y coordinate of the line to render
    ///
    /// # Returns
    /// - An iterator over the characters of the rendered line
    pub fn emit_line_chars(
        &self,
        text_type: QrTextType,
        border: u8,
        invert: bool,
        nl: bool,
        y: i32,
    ) -> impl Iterator<Item = char> + use<'_, 'a> {
        let border: i32 = border as _;

        (-border..self.size() as i32 + border + 1)
            .map(move |x| (x, y))
            .map(move |(x, y)| {
                if x < self.size() as i32 + border {
                    let white = !self.get_module(x, y) ^ invert;

                    match text_type {
                        QrTextType::Ascii => {
                            if white {
                                "#"
                            } else {
                                " "
                            }
                        }
                        QrTextType::Ansi => {
                            let prev_white = if x > -border {
                                Some(self.get_module(x - 1, y))
                            } else {
                                None
                            }
                            .map(|prev_white| !prev_white ^ invert);

                            if prev_white != Some(white) {
                                if white { "\x1b[47m " } else { "\x1b[40m " }
                            } else {
                                " "
                            }
                        }
                        QrTextType::Unicode => {
                            if white == !self.get_module(x, y + 1) ^ invert {
                                if white { "\u{2588}" } else { " " }
                            } else if white {
                                "\u{2580}"
                            } else {
                                "\u{2584}"
                            }
                        }
                    }
                } else {
                    match text_type {
                        QrTextType::Ascii => {
                            if nl {
                                "\n"
                            } else {
                                ""
                            }
                        }
                        _ => {
                            if nl {
                                "\x1b[0m\n"
                            } else {
                                "\x1b[0m"
                            }
                        }
                    }
                }
            })
            .flat_map(str::chars)
    }
}
