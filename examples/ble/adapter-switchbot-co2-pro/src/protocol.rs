//! SwitchBot CO2 Sensor Pro Protocol Decoder
//!
//! The sensor broadcasts every reading passively, split across two BLE advertisement fields:
//!
//! - Service Data (16-bit UUID `0xFD3D`, SwitchBot's assigned service UUID): 3 bytes. Byte 0 bits
//!   6:0 carry an ASCII model character identifying the device among every other SwitchBot product
//!   sharing the same manufacturer id; byte 2 carries the battery level.
//! - Manufacturer Data (company id `0x0969`): 16 bytes. Bytes 8-10 carry temperature and humidity,
//!   bytes 13-14 carry the CO2 concentration.
//!
//! Format checked against [pySwitchbot's parser] and a [byte-level capture from a real sensor].
//!
//! [pySwitchbot's parser]:
//!     https://github.com/sblibs/pySwitchbot/blob/main/switchbot/adv_parsers/meter.py
//! [byte-level capture from a real sensor]:
//!     https://zenn.dev/team_soda/articles/switch-bot-meter-pro-co2-ble

/// Service Data byte 0 (bits 6:0) values identifying a CO2 Sensor Pro ("Meter Pro CO2") among
/// SwitchBot's other products, which share the same service UUID and manufacturer id. The same
/// physical sensor has been observed broadcasting either value across different advertisements:
/// the ASCII model character `'5'`, and a raw, non-printable fallback byte also recognized by
/// [pySwitchbot's `SUPPORTED_TYPES`].
///
/// [pySwitchbot's `SUPPORTED_TYPES`]:
///     https://github.com/sblibs/pySwitchbot/blob/main/switchbot/adv_parser.py
const MODEL_BYTES: [u8; 2] = [b'5', 0x15];

/// CO2 readings above this are transient parsing artifacts, not real concentrations.
const CO2_MAX_PPM: u16 = 9_999;

/// A decoded sensor reading.
#[derive(Debug, PartialEq)]
pub struct Measurement {
    pub temperature_c: f32,
    pub humidity_percent: u8,
    pub co2_ppm: Option<u16>,
    pub battery_percent: u8,
}

impl core::fmt::Display for Measurement {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "Temperature: {:.1} °C", self.temperature_c)?;
        writeln!(f, "Relative Humidity: {} %", self.humidity_percent)?;
        match self.co2_ppm {
            Some(co2) => writeln!(f, "CO2: {co2} ppm")?,
            None => writeln!(f, "CO2: -")?,
        }
        write!(f, "Battery: {} %", self.battery_percent)
    }
}

/// Whether `service_data` identifies a SwitchBot CO2 Sensor Pro.
pub fn is_co2_sensor_pro(service_data: &[u8]) -> bool {
    matches!(service_data.first(), Some(byte) if MODEL_BYTES.contains(&(byte & 0x7F)))
}

/// Decodes a reading from the Service Data and Manufacturer Data of a matching advertisement.
pub fn parse_measurement(
    service_data: &[u8],
    manufacturer_data: &[u8],
) -> Result<Measurement, &'static str> {
    let battery_percent = *service_data.get(2).ok_or("service data too short")? & 0x7F;

    let temp_decimal = *manufacturer_data
        .get(8)
        .ok_or("manufacturer data too short")?;
    let temp_sign_int = *manufacturer_data
        .get(9)
        .ok_or("manufacturer data too short")?;
    let humidity_byte = *manufacturer_data
        .get(10)
        .ok_or("manufacturer data too short")?;
    let co2_bytes: [u8; 2] = manufacturer_data
        .get(13..15)
        .and_then(|slice| slice.try_into().ok())
        .ok_or("manufacturer data too short")?;

    let sign = if temp_sign_int & 0x80 != 0 { 1.0 } else { -1.0 };
    let integer_c = (temp_sign_int & 0x7F) as f32;
    let decimal_c = (temp_decimal & 0x0F) as f32 / 10.0;
    let temperature_c = sign * (integer_c + decimal_c);
    let humidity_percent = humidity_byte & 0x7F;
    let co2_raw = u16::from_be_bytes(co2_bytes);
    let co2_ppm = (co2_raw <= CO2_MAX_PPM).then_some(co2_raw);

    Ok(Measurement {
        temperature_c,
        humidity_percent,
        co2_ppm,
        battery_percent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_approx_eq::*;

    // Real capture from a SwitchBot CO2 Sensor Pro, see:
    // https://zenn.dev/team_soda/articles/switch-bot-meter-pro-co2-ble
    const SERVICE_DATA: [u8; 3] = [0x35, 0x00, 0x64];
    const MANUFACTURER_DATA: [u8; 16] = [
        0x00, 0x00, 0x5E, 0x00, 0x53, 0x00, 0x69, 0xE4, 0x02, 0x98, 0x2C, 0x00, 0x31, 0x03, 0x87,
        0x00,
    ];

    #[test]
    fn test_is_co2_sensor_pro() {
        assert!(is_co2_sensor_pro(&SERVICE_DATA));
    }

    #[test]
    fn test_is_co2_sensor_pro_raw_byte_fallback() {
        // Same physical sensor, captured broadcasting the raw 0x15 model byte instead of the
        // ASCII '5' on a later advertisement; pySwitchbot recognizes both as METER_PRO_C.
        assert!(is_co2_sensor_pro(&[0x15, 0x00, 0x64]));
    }

    #[test]
    fn test_is_co2_sensor_pro_other_model() {
        // 'T' (0x54) is a plain Meter, not a CO2 Sensor Pro.
        assert!(!is_co2_sensor_pro(&[0x54, 0x00, 0x64]));
    }

    #[test]
    fn test_is_co2_sensor_pro_empty() {
        assert!(!is_co2_sensor_pro(&[]));
    }

    #[test]
    fn test_parse_measurement_valid() {
        let measurement = parse_measurement(&SERVICE_DATA, &MANUFACTURER_DATA).unwrap();
        assert_approx_eq!(measurement.temperature_c, 24.2, 1e-4);
        assert_eq!(measurement.humidity_percent, 44);
        assert_eq!(measurement.co2_ppm.unwrap(), 903);
        assert_eq!(measurement.battery_percent, 100);
    }

    #[test]
    fn test_parse_measurement_negative_temperature() {
        // Sign bit clear -> negative temperature; unrelated bytes reused from the valid capture.
        let mut manufacturer_data = MANUFACTURER_DATA;
        manufacturer_data[9] = 0x18; // sign clear, integer part 24
        let measurement = parse_measurement(&SERVICE_DATA, &manufacturer_data).unwrap();
        assert_approx_eq!(measurement.temperature_c, -24.2, 1e-4);
    }

    #[test]
    fn test_parse_measurement_co2_out_of_range_is_discarded() {
        let mut manufacturer_data = MANUFACTURER_DATA;
        manufacturer_data[13] = 0xFF;
        manufacturer_data[14] = 0xFF;
        let measurement = parse_measurement(&SERVICE_DATA, &manufacturer_data).unwrap();
        assert!(measurement.co2_ppm.is_none());
    }

    #[test]
    fn test_parse_measurement_short_service_data() {
        assert!(parse_measurement(&[0x35, 0x00], &MANUFACTURER_DATA).is_err());
    }

    #[test]
    fn test_parse_measurement_short_manufacturer_data() {
        assert!(parse_measurement(&SERVICE_DATA, &MANUFACTURER_DATA[..10]).is_err());
    }
}
