//! Ruuvi Protocol Decoder
//!
//! This supports the RuuviTag Pro, which uses [Data format 5 (RAWv2)]
//!
//! [Data format 5 (RAWv2)]: https://docs.ruuvi.com/communication/bluetooth-advertisements/data-format-5-rawv2

use nom::IResult;
use nom::Parser;
use nom::bits::bits;
use nom::bits::complete::take;
use nom::combinator::verify;
use nom::number::Endianness;
use nom::number::complete::{i16, u8, u16};
use serde::{Deserialize, Serialize};

/// Measurement sample
///
/// This format covers both measurement obtained via advertisement and GATT
#[derive(Serialize, Deserialize)]
pub struct Measurement {
    pub temperature_c: Option<f32>,
    pub humidity_percent: Option<f32>,
    pub pressure_pa: Option<u32>,
    pub acc_x_mg: Option<f32>,
    pub acc_y_mg: Option<f32>,
    pub acc_z_mg: Option<f32>,
    pub power_v: Option<f32>,
    pub power_tx: Option<i8>,
    pub mov_count: Option<u8>,
    pub seq: Option<u16>,
}

impl core::fmt::Display for Measurement {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(temp) = self.temperature_c {
            writeln!(f, "Temperature: {:.3} °C", temp)?;
        } else {
            writeln!(f, "Temperature: - °C")?;
        }
        if let Some(hum) = self.humidity_percent {
            writeln!(f, "Relative Humidity: {:.4} %", hum)?;
        } else {
            writeln!(f, "Relative Humidity: - %")?;
        }
        if let Some(press) = self.pressure_pa {
            writeln!(f, "Atmospheric Pressure: {} Pa", press)?;
        } else {
            writeln!(f, "Atmospheric Pressure: - Pa")?;
        }
        if let Some(acc_x) = self.acc_x_mg {
            writeln!(f, "Acceleration X-Axis: {:.4} g", acc_x)?;
        } else {
            writeln!(f, "Acceleration X-Axis: - g")?;
        }
        if let Some(acc_y) = self.acc_y_mg {
            writeln!(f, "Acceleration Y-Axis: {:.4} g", acc_y)?;
        } else {
            writeln!(f, "Acceleration Y-Axis: - g")?;
        }
        if let Some(acc_z) = self.acc_z_mg {
            writeln!(f, "Acceleration Z-Axis: {:.4} g", acc_z)?;
        } else {
            writeln!(f, "Acceleration Z-Axis: - g")?;
        }
        if let Some(pwr_v) = self.power_v {
            writeln!(f, "Battery Voltage: {:.4} V", pwr_v)?;
        } else {
            writeln!(f, "Battery Voltage: - V")?;
        }
        if let Some(pwr_tx) = self.power_tx {
            writeln!(f, "TX Power: {} dBm", pwr_tx)?;
        } else {
            writeln!(f, "TX Power: - dBm")?;
        }
        if let Some(mov) = self.mov_count {
            writeln!(f, "Movement Count: {}", mov)?;
        } else {
            writeln!(f, "Movement Count: -")?;
        }
        if let Some(seq) = self.seq {
            writeln!(f, "Sequence: {}", seq)
        } else {
            writeln!(f, "Sequence: -")
        }
    }
}

/// Parses a GATT Heartbeat
pub fn parse_gatt_heartbeat(bytes: &[u8]) -> Result<Measurement, &str> {
    match parse_data_5_raw_v2(bytes) {
        Ok((_, measurement)) => Ok(measurement),
        Err(_) => Err("Failed to parse gatt heartbeat"),
    }
}

/// Parses a measurement in Data format 5 (RAWv2)
fn parse_data_5_raw_v2(input: &[u8]) -> IResult<&[u8], Measurement> {
    // Validate data format
    let (input, _) = verify(u8, |data_format| *data_format == 0x05).parse(input)?;

    // Parse data
    let (input, temperature) = i16(Endianness::Big).parse(input)?;
    let (input, humidity) = u16(Endianness::Big).parse(input)?;
    let (input, pressure) = u16(Endianness::Big).parse(input)?;
    let (input, acc_x) = i16(Endianness::Big).parse(input)?;
    let (input, acc_y) = i16(Endianness::Big).parse(input)?;
    let (input, acc_z) = i16(Endianness::Big).parse(input)?;
    let (input, (power_v, power_tx)) =
        bits::<_, (u16, u8), nom::error::Error<(&[u8], usize)>, _, _>((
            take(11_usize),
            take(5_usize),
        ))
        .parse(input)?;
    let (input, mov_count) = u8(input)?;
    let (input, seq) = u16(Endianness::Big).parse(input)?;

    // Convert data
    let temperature_c = (temperature != i16::MIN).then_some(temperature as f32 * 0.005_f32);
    let pressure_pa = (pressure != u16::MAX).then_some(pressure as u32 + 50_000);
    let humidity_percent = (humidity != u16::MAX).then_some(humidity as f32 * 0.002_5_f32);
    let acc_x_mg = (acc_x != i16::MIN).then_some(acc_x as f32 / 1_000_f32);
    let acc_y_mg = (acc_y != i16::MIN).then_some(acc_y as f32 / 1_000_f32);
    let acc_z_mg = (acc_z != i16::MIN).then_some(acc_z as f32 / 1_000_f32);
    let power_v = (power_v < 2047).then_some(((power_v + 1_600) as f32) / 1_000_f32);
    let power_tx = (power_tx < 31).then_some((power_tx as i8 * 2) - 40);
    let mov_count = (mov_count != u8::MAX).then_some(mov_count);
    let seq = (seq != u16::MAX).then_some(seq);

    let measurement = Measurement {
        temperature_c,
        pressure_pa,
        humidity_percent,
        acc_x_mg,
        acc_y_mg,
        acc_z_mg,
        power_v,
        power_tx,
        mov_count,
        seq,
    };

    Ok((input, measurement))
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_approx_eq::*;

    #[test]
    fn test_parse_gatt_heartbeat_valid() {
        // Provided test case: https://docs.ruuvi.com/communication/bluetooth-advertisements/data-format-5-rawv2#case-valid-data
        let valid_data = [
            0x05, 0x12, 0xFC, 0x53, 0x94, 0xC3, 0x7C, 0x00, 0x04, 0xFF, 0xFC, 0x04, 0x0C, 0xAC,
            0x36, 0x42, 0x00, 0xCD,
        ];

        let measurement = parse_gatt_heartbeat(&valid_data).unwrap();
        assert_approx_eq!(measurement.temperature_c.unwrap(), 24.3);
        assert_approx_eq!(measurement.humidity_percent.unwrap(), 53.49, 1e-2);
        assert_eq!(measurement.pressure_pa.unwrap(), 100_044);
        assert_approx_eq!(measurement.acc_x_mg.unwrap(), 0.004);
        assert_approx_eq!(measurement.acc_y_mg.unwrap(), -0.004);
        assert_approx_eq!(measurement.acc_z_mg.unwrap(), 1.036);
        assert_approx_eq!(measurement.power_v.unwrap(), 2.977);
        assert_eq!(measurement.power_tx.unwrap(), 4);
        assert_eq!(measurement.mov_count.unwrap(), 66);
        assert_eq!(measurement.seq.unwrap(), 205);
    }

    #[test]
    fn test_parse_gatt_heartbeat_max() {
        // Provided test case: https://docs.ruuvi.com/communication/bluetooth-advertisements/data-format-5-rawv2#case-maximum-values
        let valid_data = [
            0x05, 0x7F, 0xFF, 0xFF, 0xFE, 0xFF, 0xFE, 0x7F, 0xFF, 0x7F, 0xFF, 0x7F, 0xFF, 0xFF,
            0xDE, 0xFE, 0xFF, 0xFE, 0xCB, 0xB8, 0x33, 0x4C, 0x88, 0x4F,
        ];

        let measurement = parse_gatt_heartbeat(&valid_data).unwrap();
        assert_approx_eq!(measurement.temperature_c.unwrap(), 163.835, 1e-3);
        assert_approx_eq!(measurement.humidity_percent.unwrap(), 163.835, 1e-2);
        assert_eq!(measurement.pressure_pa.unwrap(), 115_534);
        assert_approx_eq!(measurement.acc_x_mg.unwrap(), 32.767);
        assert_approx_eq!(measurement.acc_y_mg.unwrap(), 32.767);
        assert_approx_eq!(measurement.acc_z_mg.unwrap(), 32.767);
        assert_approx_eq!(measurement.power_v.unwrap(), 3.646);
        assert_eq!(measurement.power_tx.unwrap(), 20);
        assert_eq!(measurement.mov_count.unwrap(), 254);
        assert_eq!(measurement.seq.unwrap(), 65_534);
    }

    #[test]
    fn test_parse_gatt_heartbeat_min() {
        // Provided test case: https://docs.ruuvi.com/communication/bluetooth-advertisements/data-format-5-rawv2#case-minimum-values
        let valid_data = [
            0x05, 0x80, 0x01, 0x00, 0x00, 0x00, 0x00, 0x80, 0x01, 0x80, 0x01, 0x80, 0x01, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xCB, 0xB8, 0x33, 0x4C, 0x88, 0x4F,
        ];

        let measurement = parse_gatt_heartbeat(&valid_data).unwrap();
        assert_approx_eq!(measurement.temperature_c.unwrap(), -163.835, 1e-3);
        assert_approx_eq!(measurement.humidity_percent.unwrap(), 0.0, 1e-2);
        assert_eq!(measurement.pressure_pa.unwrap(), 50_000);
        assert_approx_eq!(measurement.acc_x_mg.unwrap(), -32.767);
        assert_approx_eq!(measurement.acc_y_mg.unwrap(), -32.767);
        assert_approx_eq!(measurement.acc_z_mg.unwrap(), -32.767);
        assert_approx_eq!(measurement.power_v.unwrap(), 1.6);
        assert_eq!(measurement.power_tx.unwrap(), -40);
        assert_eq!(measurement.mov_count.unwrap(), 0);
        assert_eq!(measurement.seq.unwrap(), 0);
    }

    #[test]
    fn test_parse_gatt_heartbeat_invalid() {
        // Provided test case: https://docs.ruuvi.com/communication/bluetooth-advertisements/data-format-5-rawv2#case-invalid-values
        let valid_data = [
            0x05, 0x80, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0xFF,
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ];

        let measurement = parse_gatt_heartbeat(&valid_data).unwrap();
        assert!(measurement.temperature_c.is_none());
        assert!(measurement.humidity_percent.is_none());
        assert!(measurement.pressure_pa.is_none());
        assert!(measurement.acc_x_mg.is_none());
        assert!(measurement.acc_y_mg.is_none());
        assert!(measurement.acc_z_mg.is_none());
        assert!(measurement.power_v.is_none());
        assert!(measurement.power_tx.is_none());
        assert!(measurement.mov_count.is_none());
        assert!(measurement.seq.is_none());
    }
}
