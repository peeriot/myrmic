//! Hardware PWM channels via the Linux sysfs interface (`/sys/class/pwm`),
//! behind [`embedded_hal::pwm::SetDutyCycle`].
//!
//! No crate currently provides an `embedded-hal` 1.0 PWM implementation for
//! Linux, so this drives the sysfs attributes directly. The period is fixed at
//! construction (from the manifest's `freq_khz`); only the duty cycle changes
//! at runtime, matching how the ESP backend configures its LEDC timers.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use embedded_hal::pwm::{Error, ErrorKind, ErrorType, SetDutyCycle};

/// sysfs root under which PWM chips appear.
const SYSFS_PWM_ROOT: &str = "/sys/class/pwm";

/// Error from a sysfs PWM operation: an I/O failure on a sysfs attribute.
#[derive(Debug)]
pub struct PwmError(pub io::Error);

impl fmt::Display for PwmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sysfs PWM write failed: {}", self.0)
    }
}

impl std::error::Error for PwmError {}

impl Error for PwmError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// One exported sysfs PWM channel with a fixed period.
///
/// [`open`](Self::open) exports the channel if needed, programs the period
/// from `freq_khz` and enables the output at 0% duty. Duty writes scale the
/// `u16` duty to nanoseconds of the fixed period.
#[derive(Debug)]
pub struct SysfsPwm {
    /// The channel directory, e.g. `/sys/class/pwm/pwmchip0/pwm0`.
    channel_dir: PathBuf,
    period_ns: u64,
}

impl SysfsPwm {
    /// Open `channel` on the sysfs PWM chip `chip` (e.g. `pwmchip0`) with a
    /// period of `1/freq_khz` ms, enabled at 0% duty.
    ///
    /// # Errors
    ///
    /// Fails if `freq_khz` is 0 or a sysfs attribute cannot be written (chip
    /// missing, channel out of range, or insufficient permissions).
    pub fn open(chip: &str, channel: u32, freq_khz: u32) -> io::Result<Self> {
        Self::open_at(Path::new(SYSFS_PWM_ROOT), chip, channel, freq_khz)
    }

    /// [`open`](Self::open) against an explicit sysfs root, for tests.
    pub fn open_at(root: &Path, chip: &str, channel: u32, freq_khz: u32) -> io::Result<Self> {
        if freq_khz == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PWM freq_khz must be at least 1",
            ));
        }
        let chip_dir = root.join(chip);
        let channel_dir = chip_dir.join(format!("pwm{channel}"));
        if !channel_dir.is_dir() {
            fs::write(chip_dir.join("export"), channel.to_string())?;
        }
        // Order matters: the kernel rejects any state where duty exceeds
        // period, so force 0% duty before (re)programming the period.
        let period_ns = u64::from(1_000_000 / freq_khz);
        fs::write(channel_dir.join("duty_cycle"), "0")?;
        fs::write(channel_dir.join("period"), period_ns.to_string())?;
        fs::write(channel_dir.join("enable"), "1")?;
        Ok(Self {
            channel_dir,
            period_ns,
        })
    }
}

impl ErrorType for SysfsPwm {
    type Error = PwmError;
}

impl SetDutyCycle for SysfsPwm {
    fn max_duty_cycle(&self) -> u16 {
        u16::MAX
    }

    fn set_duty_cycle(&mut self, duty: u16) -> Result<(), Self::Error> {
        let duty_ns = self.period_ns * u64::from(duty) / u64::from(u16::MAX);
        fs::write(self.channel_dir.join("duty_cycle"), duty_ns.to_string()).map_err(PwmError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREQ_KHZ: u32 = 25;

    /// Create a fake sysfs chip dir with `channels` pre-exported channel dirs.
    fn fake_chip(root: &Path, chip: &str, channels: u32) {
        for ch in 0..channels {
            fs::create_dir_all(root.join(chip).join(format!("pwm{ch}"))).unwrap();
        }
        fs::write(root.join(chip).join("export"), "").unwrap();
    }

    #[test]
    fn open_programs_period_from_freq_and_enables_at_zero_duty() {
        let root = tempfile::tempdir().unwrap();
        fake_chip(root.path(), "pwmchip0", 1);

        SysfsPwm::open_at(root.path(), "pwmchip0", 0, FREQ_KHZ).unwrap();

        let ch = root.path().join("pwmchip0/pwm0");
        let expected_period = u64::from(1_000_000 / FREQ_KHZ);
        assert_eq!(
            fs::read_to_string(ch.join("period")).unwrap(),
            expected_period.to_string()
        );
        assert_eq!(fs::read_to_string(ch.join("duty_cycle")).unwrap(), "0");
        assert_eq!(fs::read_to_string(ch.join("enable")).unwrap(), "1");
    }

    #[test]
    fn set_duty_cycle_scales_to_period_ns() {
        let root = tempfile::tempdir().unwrap();
        fake_chip(root.path(), "pwmchip0", 1);

        let mut pwm = SysfsPwm::open_at(root.path(), "pwmchip0", 0, FREQ_KHZ).unwrap();
        let period_ns = u64::from(1_000_000 / FREQ_KHZ);

        pwm.set_duty_cycle(pwm.max_duty_cycle()).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("pwmchip0/pwm0/duty_cycle")).unwrap(),
            period_ns.to_string()
        );

        let half = pwm.max_duty_cycle() / 2;
        pwm.set_duty_cycle(half).unwrap();
        let expected = period_ns * u64::from(half) / u64::from(u16::MAX);
        assert_eq!(
            fs::read_to_string(root.path().join("pwmchip0/pwm0/duty_cycle")).unwrap(),
            expected.to_string()
        );
    }

    #[test]
    fn open_exports_channel_when_dir_is_missing() {
        let root = tempfile::tempdir().unwrap();
        // Chip dir with an export attribute but no pwm1 dir: a real kernel
        // would create it on export; here the export write is recorded and the
        // subsequent attribute write fails, proving the export was attempted.
        fake_chip(root.path(), "pwmchip0", 1);

        let result = SysfsPwm::open_at(root.path(), "pwmchip0", 1, FREQ_KHZ);
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("pwmchip0/export")).unwrap(),
            "1"
        );
    }

    #[test]
    fn open_rejects_zero_frequency() {
        let root = tempfile::tempdir().unwrap();
        fake_chip(root.path(), "pwmchip0", 1);

        let result = SysfsPwm::open_at(root.path(), "pwmchip0", 0, 0);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }
}
