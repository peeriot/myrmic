//! Thermobeacon Mini Hygrometer advertisement decoder
//!
//! The sensor (sold as Thermoplus / Brifit / Oria, company id `0x0010`) broadcasts every reading
//! passively in its Manufacturer Data, so nothing else is needed: no scan response, no service
//! data, no connection. The SDK hands us [`ManufacturerData::payload`] with the two company-id
//! bytes already stripped; on that buffer the layout is:
//!
//! - bytes 0-1:  frame counter / button flags (ignored here)
//! - bytes 2-7:  the sensor's MAC, little-endian (ignored here)
//! - bytes 8-9:  battery voltage in millivolts, little-endian `u16`
//! - bytes 10-11: temperature, little-endian `i16`, in sixteenths of a degree Celsius
//! - bytes 12-13: relative humidity, little-endian `u16`, in sixteenths of a percent
//!
//! Trailing bytes (an uptime counter on real advertisements) are ignored. The scaling and the
//! battery curve are ported from the ble-monitor reference parser
//! (`custom_components/ble_monitor/ble_parser/thermobeacon.py`, the `msg_length == 22` branch).

/// Smallest payload the layout requires: past the humidity field at bytes 12-13.
const MIN_PAYLOAD_LEN: usize = 14;
/// Offset of the little-endian `u16` battery voltage (millivolts).
const VOLTAGE_OFFSET: usize = 8;
/// Offset of the little-endian `i16` temperature (sixteenths of a degree Celsius).
const TEMPERATURE_OFFSET: usize = 10;
/// Offset of the little-endian `u16` humidity (sixteenths of a percent).
const HUMIDITY_OFFSET: usize = 12;
/// Plausible operating range, used to reject a corrupt or foreign advertisement.
const TEMPERATURE_RANGE_C: core::ops::RangeInclusive<f32> = -40.0..=85.0;

/// A single reading decoded from one advertisement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement {
    pub temperature_c: f32,
    pub humidity_percent: f32,
    pub battery_percent: f32,
    pub voltage_v: f32,
}

impl core::fmt::Display for Measurement {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "Temperature: {:.2} °C", self.temperature_c)?;
        writeln!(f, "Humidity:    {:.1} %", self.humidity_percent)?;
        writeln!(f, "Battery:     {:.0} %", self.battery_percent)?;
        write!(f, "Voltage:     {:.3} V", self.voltage_v)
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

/// Decodes one Thermobeacon Mini advertisement payload, company id already stripped.
pub fn parse_measurement(payload: &[u8]) -> Result<Measurement, ParseError> {
    if payload.len() < MIN_PAYLOAD_LEN {
        return Err(ParseError::TooShort);
    }

    let voltage_mv = u16::from_le_bytes([payload[VOLTAGE_OFFSET], payload[VOLTAGE_OFFSET + 1]]);
    let temperature_raw =
        i16::from_le_bytes([payload[TEMPERATURE_OFFSET], payload[TEMPERATURE_OFFSET + 1]]);
    let humidity_raw = u16::from_le_bytes([payload[HUMIDITY_OFFSET], payload[HUMIDITY_OFFSET + 1]]);

    let temperature_c = f32::from(temperature_raw) / 16.0;
    let humidity_percent = f32::from(humidity_raw) / 16.0;
    let voltage_v = f32::from(voltage_mv) / 1000.0;

    if !TEMPERATURE_RANGE_C.contains(&temperature_c) || humidity_percent > 100.0 {
        return Err(ParseError::OutOfRange);
    }

    Ok(Measurement {
        temperature_c,
        humidity_percent,
        battery_percent: battery_percent_from_millivolts(voltage_mv),
        voltage_v,
    })
}

/// Estimates the remaining charge from the battery voltage in millivolts.
///
/// A piecewise-linear curve ported from the ble-monitor reference parser: the coin cell sits near
/// full above 3.0 V and falls off through three progressively steeper segments before it is
/// considered flat.
fn battery_percent_from_millivolts(millivolts: u16) -> f32 {
    let mv = f32::from(millivolts);
    if mv >= 3000.0 {
        100.0
    } else if mv >= 2600.0 {
        60.0 + (mv - 2600.0) * 0.1
    } else if mv >= 2500.0 {
        40.0 + (mv - 2500.0) * 0.2
    } else if mv >= 2450.0 {
        20.0 + (mv - 2450.0) * 0.4
    } else {
        0.0
    }
}

/// Minimum, maximum and mean of one field across a window of readings.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Summary {
    pub min: f32,
    pub max: f32,
    pub mean: f32,
}

/// A summary of several [`Measurement`]s collected over one publish window.
///
/// Temperature and humidity are reported as full [`Summary`]s so a consumer can see how much the
/// reading moved during the window. Voltage and battery are reported as their lowest value: the
/// worst case is the useful one for a low-battery alert, and it matches how the raw battery curve
/// is treated elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Aggregate {
    pub temperature_c: Summary,
    pub humidity_percent: Summary,
    /// Lowest voltage seen in the window.
    pub voltage_v: f32,
    /// Lowest battery estimate seen in the window. Published under the
    /// `battery_percentage` key for the `sensor` event's consumers.
    #[serde(rename = "battery_percentage")]
    pub battery_percent: f32,
    /// How many readings the summary is built from.
    pub sample_count: u32,
}

/// Summarises a window of readings, or `None` when the window is empty (nothing to publish).
pub fn aggregate<'a>(measurements: impl IntoIterator<Item = &'a Measurement>) -> Option<Aggregate> {
    let mut measurements = measurements.into_iter();
    let first = measurements.next()?;

    let mut temperature_min = first.temperature_c;
    let mut temperature_max = first.temperature_c;
    let mut temperature_sum = first.temperature_c;
    let mut humidity_min = first.humidity_percent;
    let mut humidity_max = first.humidity_percent;
    let mut humidity_sum = first.humidity_percent;
    let mut voltage_v = first.voltage_v;
    let mut battery_percent = first.battery_percent;
    let mut sample_count = 1u32;

    for m in measurements {
        temperature_min = temperature_min.min(m.temperature_c);
        temperature_max = temperature_max.max(m.temperature_c);
        temperature_sum += m.temperature_c;
        humidity_min = humidity_min.min(m.humidity_percent);
        humidity_max = humidity_max.max(m.humidity_percent);
        humidity_sum += m.humidity_percent;
        voltage_v = voltage_v.min(m.voltage_v);
        battery_percent = battery_percent.min(m.battery_percent);
        sample_count += 1;
    }

    let n = sample_count as f32;
    Some(Aggregate {
        temperature_c: Summary {
            min: temperature_min,
            max: temperature_max,
            mean: temperature_sum / n,
        },
        humidity_percent: Summary {
            min: humidity_min,
            max: humidity_max,
            mean: humidity_sum / n,
        },
        voltage_v,
        battery_percent,
        sample_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absolute tolerance for the scaled floating-point comparisons below.
    const EPS: f32 = 1e-4;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < EPS,
            "expected {expected}, got {actual}"
        );
    }

    /// Builds a 14-byte payload with the given raw field values in the wire layout.
    fn payload(voltage_mv: u16, temperature_raw: i16, humidity_raw: u16) -> [u8; 14] {
        let v = voltage_mv.to_le_bytes();
        let t = temperature_raw.to_le_bytes();
        let h = humidity_raw.to_le_bytes();
        [
            0x00, 0x00, // frame counter
            0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // MAC (little-endian)
            v[0], v[1], // voltage
            t[0], t[1], // temperature
            h[0], h[1], // humidity
        ]
    }

    #[test]
    fn decodes_a_reading() {
        // 2700 mV, 344/16 = 21.5 °C, 768/16 = 48.0 %RH, battery 60 + (2700-2600)*0.1 = 70 %.
        let m = parse_measurement(&payload(2700, 344, 768)).unwrap();
        assert_close(m.temperature_c, 21.5);
        assert_close(m.humidity_percent, 48.0);
        assert_close(m.voltage_v, 2.7);
        assert_close(m.battery_percent, 70.0);
    }

    #[test]
    fn decodes_a_sub_zero_temperature() {
        // -80/16 = -5.0 °C; the signed field carries the negative value.
        let m = parse_measurement(&payload(2700, -80, 768)).unwrap();
        assert_close(m.temperature_c, -5.0);
    }

    #[test]
    fn battery_curve_matches_the_reference() {
        // One sample per segment of the piecewise curve, plus the saturated ends.
        for (mv, expected) in [
            (3100u16, 100.0f32),                 // saturated full
            (3000, 100.0),                       // top of the curve
            (2700, 70.0),                        // 60 + (2700-2600)*0.1
            (2550, 50.0),                        // 40 + (2550-2500)*0.2
            (2470, 28.0),                        // 20 + (2470-2450)*0.4
            (2400, 0.0),                         // below the floor
        ] {
            let m = parse_measurement(&payload(mv, 344, 768)).unwrap();
            assert_close(m.battery_percent, expected);
        }
    }

    #[test]
    fn rejects_a_short_payload() {
        assert_eq!(parse_measurement(&[0u8; 13]), Err(ParseError::TooShort));
    }

    #[test]
    fn rejects_an_implausible_humidity() {
        // 1616/16 = 101 %RH, which the sensor cannot produce.
        assert_eq!(
            parse_measurement(&payload(2700, 344, 1616)),
            Err(ParseError::OutOfRange)
        );
    }

    #[test]
    fn rejects_an_implausible_temperature() {
        // 1600/16 = 100 °C, outside the operating range.
        assert_eq!(
            parse_measurement(&payload(2700, 1600, 768)),
            Err(ParseError::OutOfRange)
        );
    }

    #[test]
    fn ignores_trailing_bytes() {
        // Real advertisements carry an uptime counter after the humidity field.
        let base = payload(2700, 344, 768);
        let mut extended = [0u8; 18];
        extended[..14].copy_from_slice(&base);
        assert_eq!(parse_measurement(&base), parse_measurement(&extended));
    }

    fn measurement(temperature_c: f32, humidity_percent: f32, battery_percent: f32, voltage_v: f32) -> Measurement {
        Measurement { temperature_c, humidity_percent, battery_percent, voltage_v }
    }

    #[test]
    fn aggregate_of_an_empty_window_is_none() {
        let empty: &[Measurement] = &[];
        assert_eq!(aggregate(empty), None);
    }

    #[test]
    fn aggregate_of_one_reading_repeats_that_reading() {
        let m = measurement(21.5, 48.0, 70.0, 2.7);
        let agg = aggregate(&[m]).unwrap();

        assert_eq!(agg.sample_count, 1);
        for field in [agg.temperature_c.min, agg.temperature_c.max, agg.temperature_c.mean] {
            assert_close(field, 21.5);
        }
        for field in [agg.humidity_percent.min, agg.humidity_percent.max, agg.humidity_percent.mean] {
            assert_close(field, 48.0);
        }
        assert_close(agg.voltage_v, 2.7);
        assert_close(agg.battery_percent, 70.0);
    }

    #[test]
    fn aggregate_summarises_a_window() {
        // Chosen so every statistic has a clean, distinct expected value.
        let readings = [
            measurement(21.0, 47.0, 70.0, 2.70),
            measurement(23.0, 51.0, 60.0, 2.60),
            measurement(22.0, 49.0, 65.0, 2.65),
        ];
        let agg = aggregate(&readings).unwrap();

        assert_eq!(agg.sample_count, 3);
        assert_close(agg.temperature_c.min, 21.0);
        assert_close(agg.temperature_c.max, 23.0);
        assert_close(agg.temperature_c.mean, 22.0);
        assert_close(agg.humidity_percent.min, 47.0);
        assert_close(agg.humidity_percent.max, 51.0);
        assert_close(agg.humidity_percent.mean, 49.0);
        // Voltage and battery report the worst (lowest) sample in the window.
        assert_close(agg.voltage_v, 2.60);
        assert_close(agg.battery_percent, 60.0);
    }
}
