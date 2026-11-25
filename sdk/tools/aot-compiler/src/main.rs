//! `aot-compiler` CLI to compile `.wasm` files into `.aot` files
#![warn(missing_docs)]

use std::path::PathBuf;

use aot_compiler::{Target, compile, ensure_wamrc};
use clap::Parser;

/// WASM AOT compiler that compiles a `.wasm` file into an `.aot` file
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path of the .wasm file to compile
    wasm_file: PathBuf,

    /// Target for AOT compilation
    #[arg(short, long)]
    target: Target,

    /// Path of the output dir
    #[arg(short, long)]
    out_dir: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    ensure_wamrc()?;

    let args = Args::parse();

    let out_dir = match args.out_dir {
        Some(dir) => dir,
        None => std::env::current_dir()?,
    };

    let aot = compile(&args.wasm_file, args.target, &out_dir)?;
    println!("AOT file {} generated.", aot.display());

    Ok(())
}
