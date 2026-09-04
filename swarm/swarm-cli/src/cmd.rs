use std::path::PathBuf;

#[derive(Debug, clap::Parser)]
pub struct Args {
    #[clap(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, clap::Subcommand)]
pub enum Cmd {
    Spawn(Spawn),
    /// Evaluates the input jsonnet files, and prints out the raw JSON. (This can be used to see what changes your modifications would do.)
    Eval(Spawn),
    /// This will show you exactly what zenoh will see as raw JSON. (This can be used to see any specific defaults that zenoh has, etc)
    Show(Spawn),
}

#[derive(Debug, clap::Parser)]
pub struct Spawn {
    pub config: PathBuf,

    #[arg(long, default_value = "10s")]
    pub graceful_shutdown: humantime::Duration,
}
