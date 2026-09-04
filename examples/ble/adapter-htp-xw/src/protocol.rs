//! SensorPush HTP.xw Protocol Decoder
//!
//! The HTP.xw exposes each measurement through its own GATT characteristic. A fresh sample is taken
//! by writing any 32-bit trigger value to the characteristic and then reading it back. Every value
//! is returned as a little-endian signed 32-bit integer in hundredths of the measured unit
//! (temperature in 0.01 degrees C, relative humidity in 0.01 %RH, barometric pressure in 0.01 Pa).
//!
//! See the [SensorPush Bluetooth API].
//!
//! [SensorPush Bluetooth API]: https://www.sensorpush.com/bluetooth-api

/// Decodes a raw characteristic value into a scaled measurement.
///
/// The four leading bytes are read as a little-endian `i32` in hundredths of the unit and divided
/// by 100. Trailing bytes, if any, are ignored.
pub fn parse_centi_i32(bytes: &[u8]) -> Result<f32, &'static str> {
    let raw: [u8; 4] = bytes
        .get(..4)
        .and_then(|slice| slice.try_into().ok())
        .ok_or("expected at least 4 bytes")?;
    let hundredths = i32::from_le_bytes(raw);

    Ok(hundredths as f32 / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_approx_eq::*;

    #[test]
    fn test_parse_temperature() {
        // 1970 hundredths of a degree -> 19.70 degrees C
        let value = parse_centi_i32(&[0xB2, 0x07, 0x00, 0x00]).unwrap();
        assert_approx_eq!(value, 19.70, 1e-4);
    }

    #[test]
    fn test_parse_negative_temperature() {
        // -1250 hundredths of a degree -> -12.50 degrees C
        let value = parse_centi_i32(&[0x1E, 0xFB, 0xFF, 0xFF]).unwrap();
        assert_approx_eq!(value, -12.50, 1e-4);
    }

    #[test]
    fn test_parse_humidity() {
        // 5349 hundredths of a percent -> 53.49 %RH
        let value = parse_centi_i32(&[0xE5, 0x14, 0x00, 0x00]).unwrap();
        assert_approx_eq!(value, 53.49, 1e-4);
    }

    #[test]
    fn test_parse_pressure() {
        // 10_000_000 hundredths of a Pascal -> 100_000 Pa
        let value = parse_centi_i32(&[0x80, 0x96, 0x98, 0x00]).unwrap();
        assert_approx_eq!(value, 100_000.0, 1e-1);
    }

    #[test]
    fn test_parse_extra_trailing_bytes() {
        let value = parse_centi_i32(&[0xB2, 0x07, 0x00, 0x00, 0xFF, 0xFF]).unwrap();
        assert_approx_eq!(value, 19.70, 1e-4);
    }

    #[test]
    fn test_parse_too_short() {
        assert!(parse_centi_i32(&[0x01, 0x02, 0x03]).is_err());
    }
}
