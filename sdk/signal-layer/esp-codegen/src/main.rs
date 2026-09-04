use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

use esp_codegen::generate_esp32;

#[derive(Parser)]
#[command(about = "ESP32 codegen — generates firmware pipeline_config.rs from manifest + pipeline")]
struct Args {
    /// Path to a board manifest YAML (e.g. boards/esp32c6-devkit.yaml)
    #[arg(long)]
    board: PathBuf,

    /// Path to a pipeline YAML (e.g. pipelines/basic-sensors.yaml)
    #[arg(long)]
    pipeline: PathBuf,

    /// Path to the drivers root directory (signal-modules/drivers/)
    #[arg(long)]
    drivers: PathBuf,

    /// Path to the steps root directory (signal-modules/steps/)
    #[arg(long)]
    steps: PathBuf,

    /// Output file path (e.g. `src/pipeline_config.rs`)
    #[arg(long)]
    out: PathBuf,

    /// Path to the firmware Cargo.toml to update with pipeline feature/deps.
    /// When provided, adds/updates the pipeline-{id} feature and optional deps.
    #[arg(long)]
    cargo: Option<PathBuf>,

    /// Chip feature in Cargo.toml to inject the pipeline into (e.g. esp32c6).
    #[arg(long, default_value = "esp32c6")]
    target: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let output = generate_esp32(&args.board, &args.pipeline, &args.drivers, &args.steps)?;

    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.out, &output)
        .with_context(|| format!("writing output: {}", args.out.display()))?;

    eprintln!("esp-codegen: wrote {}", args.out.display());

    if let Some(cargo_path) = &args.cargo {
        // Re-parse just for the cargo-update step (needs the raw pipeline data).
        let pipeline_yaml = std::fs::read_to_string(&args.pipeline)
            .with_context(|| format!("reading pipeline: {}", args.pipeline.display()))?;
        let pipeline: pipeline_codegen::pipeline::PipelineFile =
            serde_yaml::from_str(&pipeline_yaml)
                .with_context(|| format!("parsing pipeline: {}", args.pipeline.display()))?;

        let board_yaml = std::fs::read_to_string(&args.board)
            .with_context(|| format!("reading board manifest: {}", args.board.display()))?;
        let manifest = pipeline_codegen::manifest::parse_manifest(&board_yaml)
            .with_context(|| format!("parsing board manifest: {}", args.board.display()))?;

        pipeline_codegen::cargo_update::check_target(cargo_path, &args.target)?;
        let required = pipeline_codegen::cargo_update::required_crates(&pipeline, &manifest);
        let changed = pipeline_codegen::cargo_update::update(
            cargo_path,
            &pipeline.pipeline.id,
            &required,
            &["pipeline".to_owned()],
            &args.target,
        )?;
        if changed {
            eprintln!("esp-codegen: updated {}", cargo_path.display());
        } else {
            eprintln!("esp-codegen: {} already up-to-date", cargo_path.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use pipeline_codegen::manifest::parse_manifest;

    use esp_codegen::Esp32Backend;
    use pipeline_codegen::ChipBackend;

    fn manifest_with_gp(gp: &[u8]) -> pipeline_codegen::manifest::BoardManifest {
        manifest_for_chip("esp32c6", gp)
    }

    fn manifest_for_chip(chip: &str, gp: &[u8]) -> pipeline_codegen::manifest::BoardManifest {
        let gp_list = gp.iter().map(u8::to_string).collect::<Vec<_>>().join(", ");
        let yaml = format!(
            "id: test\n\
             chip: {chip}\n\
             buses:\n\
             \x20\x20i2c0: {{ transport: i2c, pins: {{ scl: 10, sda: 11 }}, freq_khz: 400 }}\n\
             gpios:\n\
             \x20\x20general_purpose: [{gp_list}]\n\
             devices: []\n"
        );
        parse_manifest(&yaml).expect("manifest parses")
    }

    #[test]
    fn pipeline_pins_only_exposes_pins_in_general_purpose() {
        // chip layout for esp32c6 includes GPIO0-3, 10, 11, 14, 18-23, 27.
        // Manifest reserves 10/11 as i2c bus pins. We expose only 0 and 14 via gp.
        let manifest = manifest_with_gp(&[0, 14]);
        let tokens = Esp32Backend.emit_pipeline_pins_macro(&manifest).to_string();

        // `quote!`'s Display puts whitespace around `$`, `.` so we match on the
        // unambiguous "GPIOn)" suffix that closes each `Flex::new($p.GPIOn)`.
        assert!(tokens.contains("GPIO0)"), "GPIO0 missing:\n{tokens}");
        assert!(tokens.contains("GPIO14)"), "GPIO14 missing:\n{tokens}");
        // Pins in chip layout but not in general_purpose must NOT appear.
        for pin in [1u8, 2, 3, 18, 19, 20, 21, 22, 27] {
            let needle = format!("GPIO{pin})");
            assert!(
                !tokens.contains(&needle),
                "GPIO{pin} unexpectedly exposed:\n{tokens}"
            );
        }
        // Bus pins must never appear (also not in gp).
        for pin in [10u8, 11] {
            let needle = format!("GPIO{pin})");
            assert!(!tokens.contains(&needle), "bus pin GPIO{pin} exposed");
        }
    }

    #[test]
    fn pipeline_pins_hides_device_reserved_pin_even_if_in_general_purpose() {
        // Pin 23 listed in general_purpose AND used by a device → must be `None`.
        let yaml = "\
            id: test\n\
            chip: esp32c6\n\
            buses:\n\
            \x20\x20i2c0: { transport: i2c, pins: { scl: 10, sda: 11 }, freq_khz: 400 }\n\
            gpios:\n\
            \x20\x20general_purpose: [23]\n\
            devices:\n\
            \x20\x20- id: ccs811\n\
            \x20\x20\x20\x20driver: ccs811\n\
            \x20\x20\x20\x20bus: i2c0\n\
            \x20\x20\x20\x20pins: { nint: 23 }\n";
        let manifest = parse_manifest(yaml).expect("manifest parses");
        let tokens = Esp32Backend.emit_pipeline_pins_macro(&manifest).to_string();
        assert!(
            !tokens.contains("GPIO23)"),
            "device-reserved GPIO23 should be None:\n{tokens}"
        );
    }

    #[test]
    fn validate_manifest_rejects_general_purpose_pin_outside_chip_layout() {
        // GPIO5 is not in the esp32c6 chip layout (LP / strapping subsystem).
        let manifest = manifest_with_gp(&[5]);
        let errors = Esp32Backend.validate_manifest(&manifest);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("GPIO5") && e.message.contains("not in")),
            "expected validation error for GPIO5; got: {errors:?}"
        );
    }

    #[test]
    fn validate_manifest_rejects_unsupported_chip_without_panicking() {
        let manifest = manifest_for_chip("esp32p4", &[0]);
        let errors = Esp32Backend.validate_manifest(&manifest);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("esp32p4") && e.message.contains("esp32c6")),
            "expected an unsupported-chip error naming the supported set; got: {errors:?}"
        );
    }

    #[test]
    fn validate_manifest_accepts_every_supported_chip() {
        // A pin exposed by all three layouts, so one list serves every chip.
        for chip in ["esp32c5", "esp32c6", "esp32c61"] {
            let manifest = manifest_for_chip(chip, &[0, 1]);
            let errors = Esp32Backend.validate_manifest(&manifest);
            assert!(
                errors.is_empty(),
                "{chip} should validate cleanly; got: {errors:?}"
            );
        }
    }
}
