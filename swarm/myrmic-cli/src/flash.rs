//! Flashing a built firmware onto an attached board, with `espflash` as a
//! library.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::{FlashData, FlashSettings, FlashSize, Flasher};
use espflash::image_format::ImageFormat;
use espflash::image_format::idf::IdfBootloaderFormat;
use espflash::target::{Chip, ProgressCallbacks};
use myrmic_build::firmware;
use serialport::{FlowControl, SerialPortInfo, SerialPortType, UsbPortInfo};

use crate::args::Ctx;

const BAUD: u32 = 115_200;

/// The espflash flavour of a firmware crate's chip.
pub fn espflash_chip(chip: firmware::Chip) -> Chip {
    match chip {
        firmware::Chip::Esp32c5 => Chip::Esp32c5,
        firmware::Chip::Esp32c6 => Chip::Esp32c6,
        firmware::Chip::Esp32c61 => Chip::Esp32c61,
    }
}

/// Outcome of [`choose_port`].
#[derive(Debug)]
pub enum PortChoice {
    Port(SerialPortInfo),
    /// Several USB serial ports and nothing to tell them apart; the caller
    /// asks.
    Ambiguous(Vec<SerialPortInfo>),
}

/// Picks the board's serial port: the one `requested`, else the only USB serial
/// port among `ports`.
///
/// A requested port need not appear in the listing (a pty, say) and is then
/// used as named. The listing carries real device names, never udev symlinks,
/// so a symlink is resolved before it is looked up.
pub fn choose_port(
    requested: Option<&str>,
    ports: Vec<SerialPortInfo>,
) -> anyhow::Result<PortChoice> {
    if let Some(name) = requested {
        let device = std::fs::canonicalize(name).map_or_else(
            |_| name.to_owned(),
            |path| path.to_string_lossy().into_owned(),
        );
        let port = ports
            .into_iter()
            .find(|port| port.port_name == device)
            .unwrap_or(SerialPortInfo {
                port_name: device,
                port_type: SerialPortType::Unknown,
            });
        return Ok(PortChoice::Port(port));
    }

    let mut usb: Vec<_> = ports
        .into_iter()
        .filter(|port| matches!(port.port_type, SerialPortType::UsbPort(_)))
        .collect();
    match usb.len() {
        0 => anyhow::bail!(
            "no USB serial port found; plug the board in, or name its port with --port"
        ),
        1 => Ok(PortChoice::Port(usb.remove(0))),
        _ => Ok(PortChoice::Ambiguous(usb)),
    }
}

/// `/dev/ttyACM0 (USB JTAG/serial debug unit)` — a port as shown to the user.
pub fn describe(port: &SerialPortInfo) -> String {
    match &port.port_type {
        SerialPortType::UsbPort(UsbPortInfo {
            product: Some(product),
            ..
        }) => format!("{} ({product})", port.port_name),
        _ => port.port_name.clone(),
    }
}

/// A board held in its flasher stub, ready to be asked about itself and written
/// to. Dropped unflashed — the build failed, say — it is reset so it goes back
/// to running whatever it had, instead of sitting in the stub.
pub struct Board {
    flasher: Flasher,
    flash_size: Option<FlashSize>,
    port_name: String,
    flashed: bool,
}

impl Drop for Board {
    fn drop(&mut self) {
        if !self.flashed {
            let _ = self.flasher.connection().reset();
        }
    }
}

impl Board {
    /// Resets the board into its bootloader, uploads the flasher stub, and reads
    /// what the SPI flash reports about itself.
    pub fn connect(port: &SerialPortInfo) -> anyhow::Result<Self> {
        let serial = serialport::new(&port.port_name, BAUD)
            .flow_control(FlowControl::None)
            .open_native()
            .with_context(|| format!("failed to open serial port {}", port.port_name))?;
        let usb = match &port.port_type {
            SerialPortType::UsbPort(info) => info.clone(),
            _ => UsbPortInfo {
                vid: 0,
                pid: 0,
                serial_number: None,
                manufacturer: None,
                product: None,
            },
        };
        let connection = Connection::new(
            serial,
            usb,
            ResetAfterOperation::HardReset,
            ResetBeforeOperation::DefaultReset,
            BAUD,
        );
        let mut flasher = Flasher::connect(connection, true, true, true, None, None)
            .with_context(|| format!("no ESP bootloader answered on {}", port.port_name))?;
        let flash_size = flasher.flash_detect().ok().flatten();

        Ok(Self {
            flasher,
            flash_size,
            port_name: port.port_name.clone(),
            flashed: false,
        })
    }

    pub fn chip(&self) -> Chip {
        self.flasher.chip()
    }

    /// The SPI flash's size, when the chip reports one espflash recognises.
    pub fn flash_size(&self) -> Option<FlashSize> {
        self.flash_size
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// Writes `elf` as an ESP-IDF app image alongside the stock bootloader and
    /// `partition_table` (espflash's built-in table without one), then
    /// hard-resets the board so it boots into it.
    pub fn flash(
        mut self,
        ctx: Ctx,
        elf: &Path,
        partition_table: Option<&Path>,
    ) -> anyhow::Result<()> {
        let elf_data =
            std::fs::read(elf).with_context(|| format!("failed to read {}", elf.display()))?;
        let chip = self.chip();
        let xtal_freq = chip
            .xtal_frequency(self.flasher.connection())
            .context("failed to read the board's crystal frequency")?;

        // The size stamped into the image header — detected, else espflash's
        // own 4 MB default, exactly as `espflash flash` chooses it.
        let settings = FlashSettings::new(None, Some(self.flash_size.unwrap_or_default()), None);
        let flash_data = FlashData::new(settings, 0, None, chip, xtal_freq);
        let image =
            IdfBootloaderFormat::new(&elf_data, &flash_data, partition_table, None, None, None)
                .context("failed to assemble the ESP-IDF app image")?;

        self.flasher
            .load_image_to_flash(&mut Progress { ctx }, ImageFormat::EspIdf(image))
            .context("flashing failed")?;
        self.flashed = true;
        Ok(())
    }
}

/// Reports each flashed segment through the CLI's log. A segment whose flash
/// contents already match is skipped, with `finish(true)` and no `init`.
struct Progress {
    ctx: Ctx,
}

impl ProgressCallbacks for Progress {
    fn init(&mut self, addr: u32, total: usize) {
        crate::info!(self.ctx, "Writing segment at {addr:#x} ({total} blocks)...");
    }

    fn update(&mut self, _current: usize) {}

    fn verifying(&mut self) {
        crate::debug!(self.ctx, "Verifying...");
    }

    fn finish(&mut self, skipped: bool) {
        if skipped {
            crate::info!(self.ctx, "Skipped a segment that is already up to date");
        }
    }
}

/// How long the board's port may stay away before the monitor gives up. A
/// native USB-Serial-JTAG re-enumerates on every reset, so the port vanishes
/// briefly right after flashing and on every reboot after that.
const REAPPEAR_TIMEOUT: Duration = Duration::from_secs(10);

/// Streams the board's serial output to stdout until the process is
/// interrupted, reopening the port whenever it drops.
pub fn monitor(ctx: Ctx, port_name: &str) -> anyhow::Result<()> {
    crate::info!(ctx, "Monitoring {port_name} (Ctrl-C to stop)");
    let mut stdout = std::io::stdout().lock();
    let mut buf = [0u8; 1024];
    let mut gone_since: Option<Instant> = None;

    loop {
        let mut port = match serialport::new(port_name, BAUD)
            .timeout(Duration::from_millis(100))
            .open()
        {
            Ok(port) => port,
            Err(e) => {
                let since = *gone_since.get_or_insert_with(Instant::now);
                if since.elapsed() > REAPPEAR_TIMEOUT {
                    return Err(e).with_context(|| {
                        format!("{port_name} did not come back within {REAPPEAR_TIMEOUT:?}")
                    });
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        gone_since = None;

        loop {
            match port.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    stdout.write_all(&buf[..n])?;
                    stdout.flush()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => break,
            }
        }
        crate::debug!(ctx, "{port_name} went away; waiting for it to return");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};

    fn usb(name: &str) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_owned(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid: 0x303a,
                pid: 0x1001,
                serial_number: None,
                manufacturer: None,
                product: Some("USB JTAG/serial debug unit".to_owned()),
            }),
        }
    }

    fn other(name: &str) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_owned(),
            port_type: SerialPortType::Unknown,
        }
    }

    fn chosen(choice: PortChoice) -> SerialPortInfo {
        match choice {
            PortChoice::Port(port) => port,
            PortChoice::Ambiguous(ports) => panic!("expected one port, got {ports:?}"),
        }
    }

    #[test]
    fn a_requested_port_is_taken_from_the_listing_when_present() {
        let ports = vec![usb("/dev/ttyACM0"), usb("/dev/ttyACM1")];
        let port = chosen(choose_port(Some("/dev/ttyACM1"), ports).unwrap());
        assert_eq!(port.port_name, "/dev/ttyACM1");
        assert!(matches!(port.port_type, SerialPortType::UsbPort(_)));
    }

    #[test]
    fn a_requested_port_the_listing_lacks_is_used_as_named() {
        let port = chosen(choose_port(Some("/dev/pts/9"), vec![usb("/dev/ttyACM0")]).unwrap());
        assert_eq!(port.port_name, "/dev/pts/9");
        assert!(matches!(port.port_type, SerialPortType::Unknown));
    }

    #[test]
    fn the_only_usb_port_is_chosen_on_its_own() {
        let ports = vec![other("/dev/ttyS0"), usb("/dev/ttyACM0")];
        let port = chosen(choose_port(None, ports).unwrap());
        assert_eq!(port.port_name, "/dev/ttyACM0");
    }

    #[test]
    fn no_usb_port_is_an_error_that_points_at_the_port_flag() {
        let err = format!(
            "{:#}",
            choose_port(None, vec![other("/dev/ttyS0")]).unwrap_err()
        );
        assert!(err.contains("--port"), "{err}");
    }

    #[test]
    fn several_usb_ports_are_left_for_the_caller_to_pick_from() {
        let ports = vec![
            usb("/dev/ttyACM0"),
            other("/dev/ttyS0"),
            usb("/dev/ttyACM1"),
        ];
        let PortChoice::Ambiguous(ports) = choose_port(None, ports).unwrap() else {
            panic!("expected an ambiguous choice");
        };
        let names: Vec<_> = ports.iter().map(|p| p.port_name.as_str()).collect();
        assert_eq!(names, ["/dev/ttyACM0", "/dev/ttyACM1"]);
    }

    #[test]
    fn a_port_is_described_by_its_usb_product_when_known() {
        assert_eq!(
            describe(&usb("/dev/ttyACM0")),
            "/dev/ttyACM0 (USB JTAG/serial debug unit)"
        );
        assert_eq!(describe(&other("/dev/ttyS0")), "/dev/ttyS0");
    }

    #[test]
    fn every_supported_chip_maps_to_its_espflash_namesake() {
        for chip in myrmic_build::firmware::Chip::ALL {
            assert_eq!(espflash_chip(chip).to_string(), chip.name());
        }
    }
}
