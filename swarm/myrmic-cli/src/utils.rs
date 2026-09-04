use crate::args::Ctx;
use crate::models;
use anyhow::Context as _;
use myrmic_build::cargo;

#[macro_export]
macro_rules! split {
    ($input:expr, $($delim:literal),+ $(,)?) => {{
        (|| -> Result<_, String> {
            let mut rest: &str = $input;
            let mut _idx: usize = 0;
            Ok((
                $(
                    {
                        let (head, tail) = rest.split_once($delim).ok_or_else(|| {
                            format!("split! #{}: {:?} not found in {:?}", _idx, $delim, rest)
                        })?;
                        rest = tail;
                        _idx += 1;
                        head
                    },
                )+
                rest,
            ))
        })()
    }};
}

pub fn resolve_sdk(ctx: Ctx, sdk: Option<&str>) -> anyhow::Result<models::CargoDep> {
    let sdk: std::borrow::Cow<'_, str> = if let Some(sdk) = sdk {
        std::borrow::Cow::Borrowed(sdk)
    } else if let Ok(sdk) = std::env::var(crate::MYRMIC_SDK_OVERRIDE) {
        crate::debug!(
            ctx,
            "env `{}` was set, using...",
            crate::MYRMIC_SDK_OVERRIDE
        );

        std::borrow::Cow::Owned(sdk)
    } else {
        std::borrow::Cow::Owned(crate::default_sdk()?)
    };

    let sdk = sdk.parse::<models::CargoDep>()?;

    Ok(sdk)
}

pub fn parse_from_file<T: std::str::FromStr<Err = anyhow::Error>>(
    path: &std::path::Path,
) -> anyhow::Result<T> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("unable to read: {}", path.display()))?;

    T::from_str(&content).with_context(|| format!("unable to parse file: {}", path.display()))
}

pub fn determine_name<'a>(
    name: Option<&'a str>,
    path: &'a std::path::Path,
) -> anyhow::Result<&'a str> {
    if let Some(name) = name {
        return Ok(name);
    }

    let file_name = path.file_name().with_context(|| {
        format!(
            "Cannot auto-detect name from {}. (use --name to override)",
            path.display()
        )
    })?;

    file_name
        .to_str()
        .context("cannot create a new cell with a non-unicode name")
}

impl Ctx {
    pub async fn session(self) -> anyhow::Result<zenoh::Session> {
        let mut zenoh_config = zenoh::Config::default();
        zenoh_config
            .set_mode(Some(zenoh::config::WhatAmI::Peer))
            .expect("setting mode cannot fail here");

        let session = zenoh::open(zenoh_config)
            .await
            .map_err(|zen_err| sorg_common::zenoh_err!("unable to open myrmic session", zen_err))?;

        // Self-describe on the network, so listings show this invocation as a
        // CLI instead of a bare id. Purely cosmetic — never fails the command.
        let info = introspection_client::v1::ParticipantInfo {
            kind: "cli".to_owned(),
            name: command_path(),
            origin: origin(),
        };
        if let Err(err) = introspection_client::v1::declare_participant(&session, info).await {
            crate::debug!(self, "not self-describing on the network: {err}");
        }

        wait_for_nodes(self, &session).await?;

        Ok(session)
    }
}

/// The invoked subcommand path — `"m db backup"` — as the binary name followed
/// by the subcommands clap resolved, canonical names rather than the aliases
/// typed.
///
/// Deliberately *only* the path. This is published to the whole network, and
/// argument values routinely carry credentials (`--endpoint https://key:secret@…`),
/// internal hostnames and local paths; taking the names from clap's own command
/// tree means no value the operator typed can end up on the wire. Degrades to
/// the binary name alone if argv doesn't parse.
pub(crate) fn command_path() -> String {
    use clap::CommandFactory as _;

    let mut argv = std::env::args_os();
    let bin = argv.next().map_or_else(
        || "myrmic".to_owned(),
        |arg0| {
            std::path::Path::new(&arg0)
                .file_name()
                .unwrap_or(arg0.as_os_str())
                .to_string_lossy()
                .into_owned()
        },
    );

    let Ok(matches) =
        crate::args::Args::command().try_get_matches_from(std::env::args_os().collect::<Vec<_>>())
    else {
        return bin;
    };

    let mut path = vec![bin];
    let mut level = &matches;
    while let Some((name, sub)) = level.subcommand() {
        path.push(name.to_owned());
        level = sub;
    }

    path.join(" ")
}

/// `user@host` of the invoking shell, degrading to whichever half is known.
pub(crate) fn origin() -> Option<String> {
    // `USER` is unset under systemd, cron and minimal containers, and `sudo`
    // clears it; `LOGNAME` is the conventional fallback.
    let user = ["USER", "LOGNAME"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|user| !user.is_empty());

    match (user, hostname()) {
        (Some(user), Some(host)) => Some(format!("{user}@{host}")),
        (part @ Some(_), None) | (None, part @ Some(_)) => part,
        (None, None) => None,
    }
}

fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for writes of its whole length.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) };
    if rc != 0 {
        return None;
    }

    let len = buf.iter().position(|&byte| byte == 0)?;
    let host = std::str::from_utf8(&buf[..len]).ok()?;
    (!host.is_empty()).then(|| host.to_owned())
}

const SCOUT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
/// Floor for the per-ping reply window, so a very small `--timeout` still leaves
/// each ping room to complete a local round-trip.
const MIN_PING_WINDOW: std::time::Duration = std::time::Duration::from_millis(100);
/// Roughly how many discovery pings we aim to fit inside the overall budget.
const PING_ATTEMPTS: u32 = 5;

/// Per-ping reply window derived from the overall discovery `timeout`.
///
/// Each ping is a fresh query; the window caps how long we wait for a reply
/// before re-issuing, which is how a node that finishes scouting late gets
/// caught. A fixed cap would make a larger `--timeout` pointless on slow links
/// whose reply RTT exceeds it, so the window scales with the budget (down to a
/// floor for tiny budgets).
fn ping_window(timeout: std::time::Duration) -> std::time::Duration {
    (timeout / PING_ATTEMPTS).max(MIN_PING_WINDOW)
}

/// `zenoh::open` returns while scouting still runs in the background; anything
/// sent before the first node is discovered silently misses it. Blocks until a
/// node answers a ping — a connected transport isn't proof enough, since a
/// peer can also be a gateway or another CLI.
async fn wait_for_nodes(ctx: Ctx, session: &zenoh::Session) -> anyhow::Result<()> {
    let timeout = ctx.timeout.map_or(SCOUT_TIMEOUT, Into::into);
    let window = ping_window(timeout);
    let db = db_client::v1::Client::new(session);

    tokio::time::timeout(timeout, async {
        loop {
            // Any reply proves a node is reachable, even an error.
            if let Ok(Ok(_)) = tokio::time::timeout(window, db.ping()).await {
                return;
            }

            tokio::time::sleep(MIN_PING_WINDOW).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "no myrmic nodes discovered within {} (extend with --timeout)",
            humantime::format_duration(timeout)
        )
    })
}

pub fn determine_wd(
    ctx: Ctx,
    path: Option<std::path::PathBuf>,
) -> anyhow::Result<std::path::PathBuf> {
    let path = if let Some(path) = path {
        path
    } else {
        std::env::current_dir().context("unable to determine current working directory")?
    };

    let path = path
        .canonicalize()
        .with_context(|| format!("file/folder not found: {}", path.display()))?;

    crate::debug!(ctx, "cwd = {}", path.display());

    Ok(path)
}

#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum PathType {
    Yaml,
    Toml,
    Wasm,
    Nest,
}

impl PathType {
    pub fn from_path(
        path: &std::path::Path,
    ) -> anyhow::Result<(std::borrow::Cow<'_, std::path::Path>, Self)> {
        if path.is_dir() {
            let info = cargo::crate_info(path).with_context(|| {
                format!(
                    "unable to determine path type from context: {}",
                    path.display()
                )
            })?;

            return Ok((std::borrow::Cow::Owned(info.manifest_path), Self::Toml));
        }

        let ty = Self::from_ext(path)
            .ok_or_else(|| anyhow::anyhow!("unable to determine extension: {}", path.display()))?;

        Ok((std::borrow::Cow::Borrowed(path), ty))
    }

    fn from_ext(path: &std::path::Path) -> Option<Self> {
        let ext = path.extension()?;

        if ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml") {
            Some(Self::Yaml)
        } else if ext.eq_ignore_ascii_case("nest") {
            Some(Self::Nest)
        } else if ext.eq_ignore_ascii_case("toml") {
            Some(Self::Toml)
        } else if ext.eq_ignore_ascii_case("wasm") {
            Some(Self::Wasm)
        } else {
            None
        }
    }
}

pub(crate) fn build_filter(ctx: Ctx) -> Option<String> {
    if std::env::var_os("RUST_LOG").is_some() {
        return None;
    }

    let level = match ctx.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };

    // don't @ me
    let filter = format!(
        "{0},h2=warn,sorg_execution=warn,sorg_execution::wasm::host_functions::logging={0},sorg_common=warn,db=warn,db_client=warn,wasmtime=off,cranelift_codegen=off,zenoh=off,swarm_telemetry=off,opentelemetry_sdk=off,hyper_util=off,rustls=off",
        level,
    );
    Some(filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn ping_window_scales_with_timeout() {
        // the default budget keeps the historical 100 ms window
        assert_eq!(ping_window(SCOUT_TIMEOUT), Duration::from_millis(100));
        // a larger --timeout widens the window so high-RTT replies aren't cut off
        assert_eq!(ping_window(Duration::from_secs(10)), Duration::from_secs(2));
    }

    #[test]
    fn ping_window_honours_its_floor() {
        // a tiny --timeout still leaves room for a local round-trip
        assert_eq!(ping_window(Duration::from_millis(50)), MIN_PING_WINDOW);
    }
}
