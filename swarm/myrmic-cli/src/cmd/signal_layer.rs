//! `myrmic signal-layer`: the Signal Layer scaffolds.

use crate::args::Ctx;

mod driver;
mod module;
mod new;
mod pins;
mod step;

#[derive(clap::Parser)]
pub struct SignalLayer {
    #[clap(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand)]
enum Cmd {
    /// Scaffold a pipeline project: a standalone Linux pipeline, or with
    /// --firmware an ESP32 firmware that generates the pipeline into its image.
    New(new::New),
    /// Scaffold a driver module: talks to a device on a bus and produces taps.
    Driver(driver::Driver),
    /// Scaffold a step module: processes taps into new taps.
    Step(step::Step),
}

pub fn handle(ctx: &Ctx, cmd: SignalLayer) -> anyhow::Result<()> {
    match cmd.cmd {
        Cmd::New(cmd) => new::handle(ctx, cmd),
        Cmd::Driver(cmd) => driver::handle(ctx, cmd),
        Cmd::Step(cmd) => step::handle(ctx, cmd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use myrmic_build::firmware::Chip;

    fn new_firmware(args: &[&str]) -> Option<Chip> {
        match SignalLayer::try_parse_from(args).unwrap().cmd {
            Cmd::New(new) => new.firmware,
            _ => panic!("not `new`"),
        }
    }

    #[test]
    fn new_is_a_linux_pipeline_unless_a_firmware_is_asked_for() {
        assert_eq!(new_firmware(&["signal-layer", "new", "demo"]), None);
        assert_eq!(
            new_firmware(&["signal-layer", "new", "demo", "--firmware"]),
            Some(Chip::Esp32c6)
        );
        assert_eq!(
            new_firmware(&["signal-layer", "new", "--firmware=esp32c5", "demo"]),
            Some(Chip::Esp32c5)
        );
    }

    #[test]
    fn driver_requires_a_bus() {
        assert!(SignalLayer::try_parse_from(["signal-layer", "driver", "bme280"]).is_err());

        let cmd = SignalLayer::try_parse_from(["signal-layer", "driver", "bme280", "--bus", "spi"])
            .unwrap();
        assert!(matches!(cmd.cmd, Cmd::Driver(driver) if driver.bus == module::Bus::Spi));
    }

    /// A driver crate depends on no myrmic crate, so it has no `--sdk`; a step
    /// depends on `signal-layer-core` and takes one.
    #[test]
    fn only_a_step_takes_an_sdk() {
        assert!(
            SignalLayer::try_parse_from([
                "signal-layer",
                "driver",
                "x",
                "--bus",
                "i2c",
                "--sdk",
                "0.2.1"
            ])
            .is_err()
        );
        assert!(
            SignalLayer::try_parse_from(["signal-layer", "step", "x", "--sdk", "0.2.1"]).is_ok()
        );
    }

    #[test]
    fn bus_accepts_i2c_and_spi_only() {
        for bus in ["i2c", "spi"] {
            assert!(
                SignalLayer::try_parse_from(["signal-layer", "driver", "x", "--bus", bus]).is_ok()
            );
        }
        assert!(
            SignalLayer::try_parse_from(["signal-layer", "driver", "x", "--bus", "uart"]).is_err()
        );
    }

    #[test]
    fn sl_is_a_spelling_of_signal_layer() {
        let args = crate::args::Args::try_parse_from(["myrmic", "sl", "step", "avg"]).unwrap();
        assert!(matches!(args.command, crate::args::Command::SignalLayer(_)));
    }
}
