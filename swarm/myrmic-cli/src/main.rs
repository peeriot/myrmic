use utils::*;

/// The git repository hosting `myrmic_sdk`.
const MYRMIC_SDK_GIT_URL: &str = "ssh://git@github.com/peeriot/swarm.git";
const MYRMIC_SDK_OVERRIDE: &str = "PEERIOT_MYRMIC_SDK";

/// The default `myrmic_sdk` dependency for scaffolded cells, baked in at build
/// time: a release sets `MYRMIC_SDK_VERSION` to the published SDK release it
/// ships alongside; otherwise the swarm repo pinned to the revision this CLI
/// was built from (see `build.rs`).
///
/// Errors when neither is known, so scaffolding never silently pins to a
/// guessed revision — pass `--sdk` or set `PEERIOT_MYRMIC_SDK` in that case.
fn default_sdk() -> anyhow::Result<String> {
    default_sdk_from(
        option_env!("MYRMIC_SDK_VERSION"),
        option_env!("MYRMIC_GIT_HASH"),
    )
}

fn default_sdk_from(version: Option<&str>, rev: Option<&str>) -> anyhow::Result<String> {
    if let Some(version) = version {
        return Ok(version.to_owned());
    }

    let rev = rev.ok_or_else(|| {
        anyhow::anyhow!(
            "could not determine the myrmic SDK revision (this CLI was built without VCS info); \
             pass --sdk <version|git-url|path> or set {MYRMIC_SDK_OVERRIDE}"
        )
    })?;

    Ok(format!("{MYRMIC_SDK_GIT_URL}?rev={rev}"))
}

mod archive;
mod args;
mod build;
mod deploy;
mod log;
mod models;
mod nest;
mod payload;
mod pid;
mod platforms;
mod render;
mod spawn_patch;
mod utils;

mod cmd {
    pub mod build;
    pub mod cells;
    pub mod database;
    pub mod delete;
    pub mod deploy;
    pub mod gateway;
    pub mod network;
    pub mod new;
    pub mod platforms;
    pub mod publish;
    pub mod replicate;
    pub mod runtimes;
    pub mod send;
    pub mod subscribe;
    pub mod tags;
    #[cfg(feature = "telemetry")]
    pub mod telemetry;
}

/// We do this separately as the runtime functionality can fork the entire process.
/// If we setup a tokio runtime via `tokio::main`, it can do some weird stuff.
/// So best to just avoid that and only spawn this on the top of level of the command handling.
/// (deferring inwards if we can't safely do it here (ie runtimes))
fn block_on<F, R>(fut: F) -> R
where
    F: Future<Output = R>,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .build()
        .expect("unable to build tokio runtime");

    rt.block_on(fut)
}

fn main() -> Result<(), ()> {
    // Die quietly when a pipe closes (`m rt logs | head`) instead of
    // panicking on the next print. Rust ignores SIGPIPE by default; the
    // runtime restores that before spawning (see `runtimes::start`).
    // SAFETY: installing a standard signal disposition before any threads exist.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let args::Args { ctx, command } = clap::Parser::parse();

    let result = match command {
        // Project
        args::Command::New(cmd) => cmd::new::handle(ctx, cmd),
        args::Command::Build(cmd) => cmd::build::handle(ctx, cmd),
        // Management
        args::Command::Send(cmd) => block_on(cmd::send::handle(ctx, cmd)),
        args::Command::Publish(cmd) => block_on(cmd::publish::handle(ctx, cmd)),
        args::Command::Subscribe(cmd) => block_on(cmd::subscribe::handle(ctx, cmd)),
        args::Command::Delete(cmd) => block_on(cmd::delete::handle(ctx, cmd)),
        args::Command::Deploy(cmd) => block_on(cmd::deploy::handle(ctx, cmd)),
        args::Command::Gateway(cmd) => block_on(cmd::gateway::handle(ctx, cmd)),
        args::Command::Cells(cmd) => block_on(cmd::cells::handle(ctx, cmd)),
        args::Command::Network(cmd) => block_on(cmd::network::handle(ctx, cmd)),
        #[cfg(feature = "telemetry")]
        args::Command::Telemetry(cmd) => block_on(cmd::telemetry::handle(ctx, cmd)),
        args::Command::Runtimes(cmd) => cmd::runtimes::handle(ctx, cmd),
        args::Command::Database(cmd) => block_on(cmd::database::handle(ctx, cmd)),
        args::Command::Replicate(cmd) => block_on(cmd::replicate::handle(ctx, cmd)),
        args::Command::Tags(cmd) => block_on(cmd::tags::handle(ctx, cmd)),
        // Custom
        args::Command::Platforms(cmd) => cmd::platforms::handle(ctx, cmd),
    };

    if let Err(ref err) = result {
        error!(ctx, "{}", format_error(err));
    }

    // We want to return a correct error code, but we don't want to log the error message (we already did that)
    result.map_err(|_| ())
}

fn format_error(err: &anyhow::Error) -> String {
    use std::fmt::Write as _;

    let mut out = format!("{}", err);
    for cause in err.chain().skip(1) {
        let _ = write!(out, "\nCaused by: {}", cause);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sdk_prefers_the_baked_release_version() {
        let sdk = default_sdk_from(Some("0.2.1"), Some("abc12345")).unwrap();
        assert_eq!(sdk, "0.2.1");
    }

    #[test]
    fn default_sdk_falls_back_to_the_build_revision() {
        let sdk = default_sdk_from(None, Some("abc12345")).unwrap();
        assert_eq!(sdk, "ssh://git@github.com/peeriot/swarm.git?rev=abc12345");
    }

    #[test]
    fn default_sdk_errors_without_a_version_or_revision() {
        assert!(default_sdk_from(None, None).is_err());
    }
}
