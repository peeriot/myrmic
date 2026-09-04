//! Govee H5075 Thermo-Hygrometer Protocol Decoder
//!
//! The sensor broadcasts every reading passively in the Manufacturer Data of its primary
//! advertisement (company id `0xEC88`), so nothing else is needed: no scan response, no service
//! data, no connection. The payload is 6 bytes:
//!
//! - byte 0: reserved, observed as `0x00`
//! - bytes 1-3: a 24-bit big-endian value packing both measurements. Bit 23 is a sign flag for
//!   the temperature; the remaining 23 bits hold temperature in ten-thousandths of a degree, with
//!   humidity in the last three decimal digits.
//! - byte 4: battery charge in percent
//!
//! Format checked against [GoveeBTTempLogger's decoder] and a live H5075.
//!
//! [GoveeBTTempLogger's decoder]:
//!     https://github.com/wcbonner/GoveeBTTempLogger

use serde::{Deserialize, Serialize};

/// Smallest payload the layout allows: reserved byte, 24-bit value, battery.
const MIN_PAYLOAD_LEN: usize = 5;
/// Sign flag for the temperature, bit 23 of the packed value.
const SIGN_MASK: u32 = 0x0080_0000;
/// The measurement bits below the sign flag.
const VALUE_MASK: u32 = 0x007F_FFFF;
/// Operating range of the sensor, used to reject a corrupt advertisement.
const TEMPERATURE_RANGE_C: core::ops::RangeInclusive<f32> = -40.0..=85.0;

/// A single reading decoded from one advertisement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub temperature_c: f32,
    pub humidity_percent: f32,
    pub battery_percent: u8,
}

impl core::fmt::Display for Measurement {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "Temperature: {:.2} °C", self.temperature_c)?;
        writeln!(f, "Humidity:    {:.1} %", self.humidity_percent)?;
        write!(f, "Battery:     {} %", self.battery_percent)
    }
}

/// Why an advertisement could not be turned into a [`Measurement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Fewer bytes than the layout requires.
    TooShort,
    /// Decoded outside the sensor's operating range, so the packet is not trustworthy.
    OutOfRange,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooShort => write!(f, "advertisement payload is too short"),
            Self::OutOfRange => write!(f, "decoded values are outside the sensor's range"),
        }
    }
}

/// Decodes one H5075 advertisement payload, with the company identifier already stripped.
pub fn parse_measurement(payload: &[u8]) -> Result<Measurement, ParseError> {
    if payload.len() < MIN_PAYLOAD_LEN {
        return Err(ParseError::TooShort);
    }

    let packed = u32::from(payload[1]) << 16 | u32::from(payload[2]) << 8 | u32::from(payload[3]);
    let magnitude = packed & VALUE_MASK;

    let mut temperature_c = magnitude as f32 / 10_000.0;
    if packed & SIGN_MASK != 0 {
        temperature_c = -temperature_c;
    }
    let humidity_percent = (magnitude % 1000) as f32 / 10.0;

    if !TEMPERATURE_RANGE_C.contains(&temperature_c) || humidity_percent > 100.0 {
        return Err(ParseError::OutOfRange);
    }

    Ok(Measurement {
        temperature_c,
        humidity_percent,
        battery_percent: payload[4],
    })
}

#[cfg(test)]
mod tests {
    use assert_approx_eq::assert_approx_eq;

    use super::*;

    /// `0x03DB2A` = 252_714, so 25.2714 °C and 71.4 %RH, with 88 % battery.
    #[test]
    fn decodes_a_reading() {
        let m = parse_measurement(&[0x00, 0x03, 0xDB, 0x2A, 0x58]).unwrap();
        assert_approx_eq!(m.temperature_c, 25.2714, 0.0001);
        assert_approx_eq!(m.humidity_percent, 71.4, 0.05);
        assert_eq!(m.battery_percent, 88);
    }

    /// The sign flag negates the temperature and leaves the humidity digits alone.
    #[test]
    fn sign_flag_negates_only_the_temperature() {
        let positive = parse_measurement(&[0x00, 0x03, 0xDB, 0x2A, 0x58]).unwrap();
        let negative = parse_measurement(&[0x00, 0x83, 0xDB, 0x2A, 0x58]).unwrap();
        assert_approx_eq!(negative.temperature_c, -positive.temperature_c, 0.0001);
        assert_approx_eq!(negative.humidity_percent, positive.humidity_percent, 0.05);
    }

    #[test]
    fn rejects_a_short_payload() {
        assert_eq!(
            parse_measurement(&[0x00, 0x03, 0xDB, 0x2A]),
            Err(ParseError::TooShort)
        );
    }

    /// A value the sensor cannot physically produce is rejected rather than published.
    #[test]
    fn rejects_an_implausible_temperature() {
        assert_eq!(
            parse_measurement(&[0x00, 0x7F, 0xFF, 0xFF, 0x58]),
            Err(ParseError::OutOfRange)
        );
    }

    /// Trailing bytes are ignored, so a longer advertisement still decodes.
    #[test]
    fn ignores_trailing_bytes() {
        let short = parse_measurement(&[0x00, 0x03, 0xDB, 0x2A, 0x58]).unwrap();
        let long = parse_measurement(&[0x00, 0x03, 0xDB, 0x2A, 0x58, 0x00]).unwrap();
        assert_eq!(short, long);
    }
}
