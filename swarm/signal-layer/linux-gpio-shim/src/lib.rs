//! GPIO and PWM shim for the Linux signal layer: character-device GPIO lines
//! and sysfs PWM channels behind the `embedded-hal` 1.0 digital/PWM traits, so
//! the platform-agnostic output drivers (`gpio-output`, `pwm-output`, …) run
//! unchanged on Linux.

#[cfg(target_os = "linux")]
mod gpio;
mod pwm;

#[cfg(target_os = "linux")]
pub use gpio::{LinuxInputPin, LinuxOutputPin};
pub use pwm::{PwmError, SysfsPwm};
