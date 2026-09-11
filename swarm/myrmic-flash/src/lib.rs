//! Flashing a built firmware onto an attached board, with `espflash` as a
//! library: its CLI layer picks the port and connects, its progress bars report
//! the write, and it assembles the ESP-IDF app image.
//!
//! The `myrmic` CLI and the hardware-in-the-loop tests share this so both
//! assemble and write the image the same way. Only how they reach the board
//! differs: the CLI connects through [`Board::connect`] (an interactive port
//! pick), while a rig that already holds a fixed port hands its connected
//! [`Flasher`] to [`Board::from_flasher`]. From there [`Board::info`] and
//! [`Board::flash`] are one routine.

use std::path::Path;

use anyhow::Context as _;
use dialoguer::console::Term;
use espflash::cli::config::Config;
use espflash::cli::{ConnectArgs, EspflashProgress};
use espflash::flasher::{DeviceInfo, FlashData, FlashSettings, Flasher};
use espflash::image_format::ImageFormat;
use espflash::image_format::idf::IdfBootloaderFormat;
use espflash::target::Chip;
use myrmic_build::firmware;
use serialport::SerialPort as _;

/// A board held in its flasher stub, ready to be asked about itself and written
/// to. Dropped unflashed - the build failed, say - it is reset so it goes back
/// to running whatever it had, instead of sitting in the stub.
pub struct Board {
    /// Gone once [`Board::flash`] has handed the port over to the caller.
    flasher: Option<Flasher>,
}

/// The espflash flavour of a firmware crate's chip.
pub fn espflash_chip(chip: firmware::Chip) -> Chip {
    match chip {
        firmware::Chip::Esp32c5 => Chip::Esp32c5,
        firmware::Chip::Esp32c6 => Chip::Esp32c6,
        firmware::Chip::Esp32c61 => Chip::Esp32c61,
    }
}

/// An espflash diagnostic as an anyhow error, its cause chain intact and its
/// help, when it has one, folded into the message.
pub fn diagnostic(report: &miette::Report) -> anyhow::Error {
    let mut causes: Vec<String> = report.chain().map(ToString::to_string).collect();
    if let (Some(help), Some(top)) = (report.help(), causes.first_mut()) {
        *top = format!("{top} ({help})");
    }
    let mut error = anyhow::Error::msg(causes.pop().unwrap_or_default());
    while let Some(cause) = causes.pop() {
        error = error.context(cause);
    }

    error
}

/// espflash's own configuration: the `espflash.toml` of the working directory
/// and the ports its binary has been told to remember.
pub fn config() -> anyhow::Result<Config> {
    Config::load().map_err(|report| diagnostic(&report))
}

/// The outcome of an espflash step that may have shown its port picker, tidied
/// up after: a pick the user backed out of becomes a plain error.
///
/// The picker leaves behind a Ctrl+C handler that only re-shows the cursor, and
/// does so from another thread. Default handling is put back so a later Ctrl+C
/// still stops the command, and after a backed-out pick the cursor is shown
/// here rather than raced for.
pub fn picked<T>(outcome: miette::Result<T>) -> anyhow::Result<T> {
    let backed_out = outcome.as_ref().is_err_and(backed_out);
    // SAFETY: restoring a standard disposition; no handler code is involved.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
    }
    let term = Term::stderr();
    if backed_out && term.is_term() {
        let _ = term.show_cursor();
    }
    outcome.map_err(|report| {
        if backed_out {
            anyhow::anyhow!("no port chosen")
        } else {
            diagnostic(&report)
        }
    })
}

impl Drop for Board {
    fn drop(&mut self) {
        if let Some(flasher) = &mut self.flasher {
            let _ = flasher.connection().reset();
        }
    }
}

impl Board {
    /// Picks the port `args` names, or the user does, then resets the board into
    /// its bootloader and uploads the flasher stub.
    pub fn connect(args: &ConnectArgs, config: &Config) -> anyhow::Result<Self> {
        let flasher = picked(espflash::cli::connect(args, config, false, false))?;

        Ok(Self {
            flasher: Some(flasher),
        })
    }

    /// Wraps an already-connected flasher, for a caller that reached the board
    /// its own way - a fixed rig port, say - rather than through the interactive
    /// [`Board::connect`].
    pub fn from_flasher(flasher: Flasher) -> Self {
        Self {
            flasher: Some(flasher),
        }
    }

    pub fn chip(&mut self) -> Chip {
        self.flasher().chip()
    }

    /// Prints what the board says about itself, as `espflash board-info` does,
    /// and returns it. The flash size is the detected one, else espflash's 4 MB
    /// default.
    pub fn info(&mut self) -> anyhow::Result<DeviceInfo> {
        espflash::cli::print_board_info(self.flasher()).map_err(|report| diagnostic(&report))
    }

    /// Writes `elf` as an ESP-IDF app image alongside the stock bootloader and
    /// `partition_table` (espflash's built-in table without one), then
    /// hard-resets the board so it boots into it. Returns the port's name, to
    /// reopen it by once the board is back.
    pub fn flash(
        mut self,
        elf: &[u8],
        partition_table: Option<&Path>,
        info: &DeviceInfo,
    ) -> anyhow::Result<String> {
        let settings = FlashSettings::new(None, Some(info.flash_size), None);
        let flash_data = FlashData::new(settings, 0, None, self.chip(), info.crystal_frequency);
        let image = IdfBootloaderFormat::new(elf, &flash_data, partition_table, None, None, None)
            .context("failed to assemble the ESP-IDF app image")?;

        self.flasher()
            .load_image_to_flash(&mut EspflashProgress::default(), ImageFormat::EspIdf(image))
            .context("flashing failed")?;
        let flasher = self.flasher.take().expect("still connected");

        flasher
            .into_connection()
            .into_serial()
            .name()
            .context("the board's port has no name to reopen it by")
    }

    fn flasher(&mut self) -> &mut Flasher {
        self.flasher
            .as_mut()
            .expect("the board is connected until it is flashed")
    }
}

/// Escape leaves the picker as cancelled, Ctrl+C as an interrupted read.
fn backed_out(report: &miette::Report) -> bool {
    matches!(
        report.downcast_ref::<espflash::Error>(),
        Some(espflash::Error::Cancelled | espflash::Error::DialoguerError(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_chip_maps_to_its_espflash_namesake() {
        for chip in myrmic_build::firmware::Chip::ALL {
            assert_eq!(espflash_chip(chip).to_string(), chip.name());
        }
    }

    #[test]
    fn a_diagnostic_keeps_its_cause_chain() {
        use miette::{IntoDiagnostic as _, WrapErr as _};

        let report = Err::<(), _>(std::io::Error::other("inner"))
            .into_diagnostic()
            .wrap_err("outer")
            .unwrap_err();
        let chain: Vec<String> = diagnostic(&report)
            .chain()
            .map(ToString::to_string)
            .collect();
        assert_eq!(chain, ["outer", "inner"]);
    }

    #[test]
    fn a_diagnostic_carries_its_help_in_the_message() {
        let report = miette::Report::new(miette::diagnostic!(help = "try --port", "no port"));
        assert_eq!(diagnostic(&report).to_string(), "no port (try --port)");
    }
}
