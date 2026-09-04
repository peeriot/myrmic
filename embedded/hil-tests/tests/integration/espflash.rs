//! Device operations that use `espflash`
//!
//! In this case we are using `espflash` as a library, so that we have more control over the spawned
//! processes.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use espflash::cli::monitor::parser;
use espflash::cli::monitor::parser::{InputParser, ResolvingPrinter};
use espflash::cli::{EspflashProgress, make_image_format};
use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::{FlashData, FlashSettings, Flasher};
use espflash::image_format::ImageFormatKind;
use serialport::{FlowControl, SerialPortType, UsbPortInfo, available_ports};

/// Handle of the serial-monitor thread started by [`flash_device`]. Tests keep it in scope
/// (`let _monitor = ...`) so device output keeps streaming for the duration of the test.
pub type MonitorHandle = thread::JoinHandle<anyhow::Result<()>>;

/// Flash the firmware under test and start a serial monitor thread, returning its handle.
///
/// The monitor streams device output into `tracing` for as long as the handle is alive, so tests
/// keep it in scope (`let _monitor = ...`) to have the device log alongside the swarm log.
pub fn flash_device() -> anyhow::Result<MonitorHandle> {
    let elf_path = firmware_elf_path();
    if !elf_path.exists() {
        anyhow::bail!(
            "firmware ELF does not exist at {}. Build it (or point EMBEDDED_ELF at it) and re-run",
            elf_path.display()
        );
    }
    flash_and_monitor(&elf_path)
}

/// Flash the *production-profile* firmware and start a serial monitor, as [`flash_device`] does.
///
/// The suite's default image is built with `--features wdt-selftest` so the watchdog tests can
/// trigger a liveness stall. A test asserting what a production build does must not run on that
/// image, so this flashes a separate one built without the test features, published by the
/// workflow as `EMBEDDED_ELF_PRODUCTION`.
///
/// Leaving the device on the production image is safe: every test flashes at its start, so the
/// next one restores whatever it needs.
pub fn flash_production_device() -> anyhow::Result<MonitorHandle> {
    let Some(elf_path) = production_firmware_elf_path() else {
        anyhow::bail!(
            "EMBEDDED_ELF_PRODUCTION is not set. Build a firmware without `--features \
             wdt-selftest` and point this at it (see .github/workflows/hardware-tests.yml)"
        );
    };
    if !elf_path.exists() {
        anyhow::bail!(
            "production firmware ELF does not exist at {}",
            elf_path.display()
        );
    }
    flash_and_monitor(&elf_path)
}

/// Path to the production-profile firmware, or `None` when it was not published.
pub fn production_firmware_elf_path() -> Option<PathBuf> {
    std::env::var("EMBEDDED_ELF_PRODUCTION")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// Path to the firmware ELF the harness flashes.
///
/// Defaults to the release build for the selected `EMBEDDED_TARGET`'s ISA, at
/// `target/<triple>/release/modem-esp32` (see [`aot::firmware_target_triple`]). Override with
/// `EMBEDDED_ELF` to point at a different build.
fn firmware_elf_path() -> PathBuf {
    if let Ok(path) = std::env::var("EMBEDDED_ELF") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(crate::integration::aot::firmware_target_triple())
        .join("release/modem-esp32")
}

fn flash_and_monitor(elf_path: &Path) -> anyhow::Result<thread::JoinHandle<anyhow::Result<()>>> {
    EspFlash::flash_elf(elf_path.to_path_buf())?;
    let elf_bytes = std::fs::read(elf_path)
        .map_err(|e| anyhow::anyhow!("failed to read ELF {}: {e}", elf_path.display()))?;
    EspFlash::serial_monitor(elf_bytes)
}

pub struct EspFlash;

impl EspFlash {
    /// Flashes the ELF binary onto the device
    pub fn flash_elf(elf_path: PathBuf) -> anyhow::Result<()> {
        let port_name =
            std::env::var("ESPFLASH_PORT").unwrap_or_else(|_| "/dev/ttyACM0".to_owned());

        // `available_ports()` reports the real device names (e.g. /dev/ttyACM4), never udev
        // symlinks like /dev/esp32-hil-2. Resolve the symlink so the port_info lookup below finds
        // the real USB pid.
        let device_name = std::fs::canonicalize(&port_name)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| port_name.clone());

        tracing::info!(
            "Started flashing ELF {} to {device_name} (from {port_name})",
            elf_path.display()
        );

        let port_info = available_ports()
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.port_name == device_name)
            .and_then(|p| match p.port_type {
                SerialPortType::UsbPort(info) => Some(info),
                _ => None,
            })
            .unwrap_or(UsbPortInfo {
                vid: 0,
                pid: 0,
                serial_number: None,
                manufacturer: None,
                product: None,
            });

        tracing::info!("Open serial port...");

        let serial = serialport::new(&device_name, 115_200)
            .flow_control(FlowControl::None)
            .open_native()
            .map_err(|e| anyhow::anyhow!("failed to open serial port {device_name}: {e}"))?;

        tracing::info!("Create espflash connection...");

        let connection = Connection::new(
            serial,
            port_info,
            ResetAfterOperation::HardReset,
            ResetBeforeOperation::DefaultReset,
            115_200,
        );

        tracing::info!("Create espflash flasher...");

        let mut flasher = Flasher::connect(connection, true, true, true, None, None)
            .map_err(|e| anyhow::anyhow!("espflash connect failed: {e}"))?;

        tracing::info!("Read ELF file...");

        let elf_data = std::fs::read(&elf_path)
            .map_err(|e| anyhow::anyhow!("failed to read ELF {}: {e}", elf_path.display()))?;

        tracing::info!("Get chip information...");

        let chip = flasher.chip();
        let xtal_freq = chip
            .xtal_frequency(flasher.connection())
            .map_err(|e| anyhow::anyhow!("failed to get xtal frequency: {e}"))?;

        tracing::info!("Create flash data and image format...");

        let flash_data = FlashData::new(FlashSettings::default(), 0, None, chip, xtal_freq);
        let image_format = make_image_format(
            &elf_data,
            &flash_data,
            ImageFormatKind::EspIdf,
            &Default::default(),
            None,
            None,
            None,
        )
        .map_err(|e| anyhow::anyhow!("failed to create image format: {e}"))?;

        tracing::info!("Load image to flash...");

        flasher
            .load_image_to_flash(&mut EspflashProgress::default(), image_format)
            .map_err(|e| anyhow::anyhow!("load_image_to_flash failed: {e}"))?;

        tracing::info!("Flashing completed successfully!");

        Ok(())
    }

    /// Uses the serial port to monitor the device.
    ///
    /// The port is opened synchronously so that a connection failure is reported to the caller
    /// right away - otherwise the harness would wait out the full exec-registration timeout on a
    /// monitor that never attached. Only the read-polling loop runs on the background thread.
    /// The caller does not join the returned handle (the loop only ends on error), so read/EOF
    /// failures are logged from the thread to stay visible in `--no-capture` output.
    pub fn serial_monitor(
        elf_bytes: Vec<u8>,
    ) -> anyhow::Result<thread::JoinHandle<anyhow::Result<()>>> {
        let port = std::env::var("ESPFLASH_PORT").unwrap_or_else(|_| "/dev/ttyACM0".to_owned());

        let mut serial = serialport::new(&port, 115_200)
            .timeout(Duration::from_millis(5))
            .open_native()
            .map_err(|e| anyhow::anyhow!("failed to open serial port {port}: {e}"))?;
        tracing::info!("serial monitor: attached to {port}, streaming device output");

        let handle = thread::spawn(move || {
            let mut printer =
                ResolvingPrinter::new(vec![elf_bytes.as_slice()], TracingWriter::new(), false);
            let mut parser = parser::serial::Serial;
            let mut buf = [0u8; 1024];

            loop {
                match serial.read(&mut buf) {
                    Ok(0) => {
                        // A native USB-Serial-JTAG (e.g C6) re-enumerates on reset; an EOF here
                        // usually means the port dropped out from under us.
                        tracing::warn!(
                            "serial monitor: {port} returned EOF - device likely disconnected or re-enumerated"
                        );
                        return Ok(());
                    }
                    Ok(n) => {
                        parser.feed(&buf[..n], &mut printer);
                        let _ = printer.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(e) => {
                        tracing::error!("serial monitor: read from {port} failed: {e}");
                        return Err(anyhow::anyhow!(e));
                    }
                }
            }
        });

        Ok(handle)
    }
}

/// A [`std::io::Write`] sink that forwards the device's serial output to `tracing`, one line at a
/// time. Raw writes to stdout/stderr from the monitor thread are not surfaced in CI (nextest only
/// captures the `tracing` subscriber's writer, not background-thread fd writes), so we re-emit each
/// line through `tracing` - the same sink the swarm logs use, which does show up.
struct TracingWriter {
    line: Vec<u8>,
}

impl TracingWriter {
    fn new() -> Self {
        Self { line: Vec::new() }
    }
}

impl Write for TracingWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        for &byte in data {
            if byte != b'\n' {
                self.line.push(byte);
                continue;
            }
            let line = std::mem::take(&mut self.line);
            let clean = strip_ansi(&line);
            let text = String::from_utf8_lossy(&clean);
            let trimmed = text.trim_end_matches('\r');
            if !trimmed.is_empty() {
                if trimmed.contains("TRACE -") {
                    tracing::trace!("[device] {trimmed}");
                } else if trimmed.starts_with("DEBUG -") {
                    tracing::debug!("[device] {trimmed}");
                } else if trimmed.starts_with("WARN -") {
                    tracing::warn!("[device] {trimmed}");
                } else if trimmed.starts_with("ERROR -") {
                    tracing::error!("[device] {trimmed}");
                } else {
                    tracing::info!("[device] {trimmed}");
                }
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Strips ANSI CSI escape sequences (the color codes esp-println emits) so device lines log as
/// plain text instead of raw `\x1b[..m` noise.
fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                // Skip parameter/intermediate bytes up to and including the final byte (0x40..=0x7e).
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i += 1;
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}
