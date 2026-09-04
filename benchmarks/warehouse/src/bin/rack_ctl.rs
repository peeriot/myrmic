//! Precondition/postcondition helper for running the warehouse benchmark against a rack of real
//! hosts over SSH: uploads the cross-compiled `myrmic` binary every host needs before a run
//! (`upload`), and tears every host's runtime + uploaded binary back down afterwards (`cleanup`).
//! See `benchmarks/warehouse/run_rack.sh`, which drives both around `warehouse-bench` itself.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use warehouse_benchmark::rack_config::RackCtlConfig;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Upload the (already cross-compiled) `myrmic` binary to every host in `config`'s
    /// `[specialized.rack]` table, at the path that table's `myrmic_path` names.
    Upload {
        /// path to the benchmark's TOML config file (only `[specialized.rack]` and
        /// `num_objects`/`num_zones` are read).
        #[clap(long)]
        config: PathBuf,
        /// path to the cross-compiled `myrmic` binary to upload (e.g.
        /// `target/aarch64-unknown-linux-gnu/release/myrmic`).
        #[clap(long)]
        binary: PathBuf,
        /// SSH identity file (`ssh -i`/`scp -i`) to use for every host, e.g. when the rack
        /// doesn't accept the default key. Equivalent to setting `SSH_IDENTITY_FILE`.
        #[clap(short = 'i', long)]
        identity_file: Option<PathBuf>,
    },
    /// Stop every host's runtime and remove its uploaded `myrmic` binary (best-effort) — see
    /// `test_framework::rack::cleanup`.
    Cleanup {
        /// path to the benchmark's TOML config file (only `[specialized.rack]` and
        /// `num_objects`/`num_zones` are read).
        #[clap(long)]
        config: PathBuf,
        /// SSH identity file (`ssh -i`/`scp -i`) to use for every host, e.g. when the rack
        /// doesn't accept the default key. Equivalent to setting `SSH_IDENTITY_FILE`.
        #[clap(short = 'i', long)]
        identity_file: Option<PathBuf>,
    },
}

/// Makes `test_framework::ssh_identity_file()` pick up `identity_file`, if given, for every
/// SSH/SCP connection this run opens.
fn apply_identity_file(identity_file: Option<&PathBuf>) {
    if let Some(identity_file) = identity_file {
        // SAFETY: single-threaded at this point in `main`, before any SSH/SCP connection reads
        // the var.
        unsafe { std::env::set_var("SSH_IDENTITY_FILE", identity_file) };
    }
}

#[tokio::main]
async fn main() {
    match Args::parse().command {
        Command::Upload {
            config,
            binary,
            identity_file,
        } => {
            apply_identity_file(identity_file.as_ref());
            let config = RackCtlConfig::load(&config);
            let hosts = config.host_specs();
            println!(
                "uploading {} to {} host(s)...",
                binary.display(),
                hosts.len()
            );
            test_framework::rack::upload_binary(&hosts, &binary, config.myrmic_path()).await;
            println!("upload done.");
        }
        Command::Cleanup {
            config,
            identity_file,
        } => {
            apply_identity_file(identity_file.as_ref());
            let config = RackCtlConfig::load(&config);
            let hosts = config.host_specs();
            println!("cleaning up {} host(s)...", hosts.len());
            test_framework::rack::cleanup(&hosts, config.myrmic_path()).await;
            println!("cleanup done.");
        }
    }
}
