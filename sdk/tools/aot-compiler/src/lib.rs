//! Library for AOT-compiling `.wasm` files into `.aot` files via `wamrc`.
#![warn(missing_docs)]

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, format_err};
use clap::ValueEnum;
use owo_colors::OwoColorize;

/// List of AOT targets
#[derive(Debug, Copy, Clone, ValueEnum)]
#[allow(non_camel_case_types, missing_docs)]
pub enum Target {
    ESP32C5,
    ESP32C6,
    ESP32C61,
}

impl Target {
    /// Translate the target into the corresponding `wamrc` CLI flags.
    pub fn to_wamrc_args(self) -> Vec<&'static str> {
        match self {
            Self::ESP32C5 | Self::ESP32C6 | Self::ESP32C61 => vec![
                "--target=riscv32",
                "--target-abi=ilp32",
                "--cpu=generic-rv32",
                "--cpu-features=+i,+m,+a,+c",
            ],
        }
    }
}

impl FromStr for Target {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        for variant in Self::value_variants() {
            if variant.to_possible_value().unwrap().matches(s, false) {
                return Ok(*variant);
            }
        }

        Err(format!("Invalid variant {s}"))
    }
}

/// Verify that the `wamrc` binary is on `PATH`.
pub fn ensure_wamrc() -> anyhow::Result<()> {
    // Make sure `wamrc` is there
    if which::which("wamrc").is_err() {
        anyhow::bail!(
            "{}",
            "\"wamrc\" binary not found. Please install it by following the instructions at \
            https://wamr.gitbook.io/document/wamr-in-practice/tutorial/build-tutorial/build_wamrc"
                .red()
                .bold()
        );
    }
    // Also make sure it's the right version
    let output = std::process::Command::new("wamrc")
        .arg("--version")
        .output()
        .context("failed to invoke wamrc")?;
    let version =
        String::from_utf8(output.stdout).context("failed to parse wamrc version output")?;
    if !version.starts_with("wamrc 2.4.4") {
        anyhow::bail!(
            "{}",
            format!(
                "wamrc version 2.4.4 is required, but found {version}. Please install it by \
                following the instructions at \
                https://wamr.gitbook.io/document/wamr-in-practice/tutorial/build-tutorial/build_wamrc \
                and making sure you use the correct tag/release."
            )
            .red()
            .bold()
        );
    }

    Ok(())
}

/// Compile `wasm_file` for `target`, writing the `.aot` file into `out_dir`.
/// Returns its path.
pub fn compile(wasm_file: &Path, target: Target, out_dir: &Path) -> anyhow::Result<PathBuf> {
    let filename = wasm_file
        .file_stem()
        .ok_or_else(|| format_err!("not a file"))?;

    let aot_file = out_dir.join(Path::new(filename).with_extension("aot"));

    // Call wamrc to spit out AOT file
    let status = std::process::Command::new("wamrc")
        .arg("--xip")
        .args(target.to_wamrc_args())
        .args(["-o", &format!("{}", aot_file.display())])
        .arg(wasm_file.as_os_str())
        .status()
        .context("failed to invoke wamrc")?;

    if !status.success() {
        anyhow::bail!("wamrc exited with {status}");
    }

    Ok(aot_file)
}
