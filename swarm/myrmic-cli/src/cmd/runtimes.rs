use crate::args::Ctx;
use std::path::{Path, PathBuf};

use crate::pid::*;
use anyhow::Context as _;

mod delete;
mod info;
mod list;
mod logs;
mod start;

const DEFAULT_PID_DIR: &str = "myrmic";
const DEFAULT_RUNTIME_NAME: &str = "default";

#[derive(clap::Parser)]
pub struct Runtimes {
    /// Name of the runtime to operate on, e.g. `runtimes <NAME> logs`.
    ///
    /// Without a subcommand, prints that runtime's info.
    pub name: Option<String>,

    #[clap(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// List runtimes discovered started via `runtime start`
    ///
    /// Displays status (running, stale, or invalid)
    List(list::List),
    /// Start a runtime instance.
    ///
    /// Runs in the foreground by default; pass `--detached` to fork into a background daemon.
    #[clap(alias = "run")]
    Start(start::Start),
    /// Stop one or more runtimes by name.
    ///
    /// Sends SIGTERM to the runtime.
    #[clap(alias = "remove", alias = "rm", alias = "stop")]
    Delete(delete::Delete),
    /// Print a runtime's log files, optionally following as it grows.
    Logs(logs::Logs),
    /// Print a runtime's details: identity, status, data folder, and
    /// on-disk database stats.
    Info(info::Info),
}

#[allow(clippy::unnecessary_wraps)]
pub fn handle(ctx: Ctx, cmd: Runtimes) -> anyhow::Result<()> {
    let Runtimes { name, cmd } = cmd;

    // A bare name shows that runtime; nothing at all lists them.
    let cmd = cmd.unwrap_or_else(|| {
        if name.is_some() {
            Cmd::Info(info::Info {
                pid_path: None,
                name: None,
            })
        } else {
            Cmd::List(list::List {
                pid_path: None,
                name: None,
            })
        }
    });

    match cmd {
        Cmd::List(mut cmd) => {
            cmd.name = merge_names(name, cmd.name.take())?;
            list::handle(ctx, cmd)
        }
        Cmd::Start(mut cmd) => {
            cmd.name = merge_names(name, cmd.name.take())?;
            start::handle(ctx, cmd)
        }
        Cmd::Delete(mut cmd) => {
            cmd.name.extend(name);
            delete::handle(ctx, cmd)
        }
        Cmd::Logs(mut cmd) => {
            cmd.name = merge_names(name, cmd.name.take())?;
            logs::handle(ctx, cmd)
        }
        Cmd::Info(mut cmd) => {
            cmd.name = merge_names(name, cmd.name.take())?;
            info::handle(ctx, cmd)
        }
    }
}

/// A runtime name may come before the subcommand or as the subcommand's own
/// argument; giving two different ones is ambiguous.
fn merge_names(outer: Option<String>, inner: Option<String>) -> anyhow::Result<Option<String>> {
    match (outer, inner) {
        (Some(a), Some(b)) if a != b => {
            anyhow::bail!("two different runtime names given ({a:?} and {b:?})")
        }
        (outer, inner) => Ok(outer.or(inner)),
    }
}

/// Rejects names that could escape the pid directory or break filesystem layout.
fn validate_runtime_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("runtime name cannot be empty");
    }
    if name == "." || name == ".." {
        anyhow::bail!("runtime name {name:?} is reserved");
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        anyhow::bail!("runtime name {name:?} must not contain path separators");
    }
    Ok(())
}

/// Given a `path` attempts to resolve all pid files.
fn discover_pids(path: &Path) -> anyhow::Result<Option<Vec<Pid>>> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("failed to stat {}", path.display()))
            );
        }
    };

    if meta.is_file() {
        return Ok(Some(vec![Pid::from_path(path.to_path_buf())]));
    }

    if !meta.is_dir() {
        anyhow::bail!("{} is neither a file nor a directory", path.display());
    }

    let mut entries: Vec<_> = std::fs::read_dir(path)
        .with_context(|| format!("failed to read pid dir {}", path.display()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pid"))
        .collect();

    entries.sort_by_key(std::fs::DirEntry::file_name);

    let entries = entries
        .into_iter()
        .map(|e| Pid::from_path(e.path()))
        .collect();

    Ok(Some(entries))
}

/// The runtime a name-less command targets: the only one with a pid file,
/// else the only one currently running, else "default" when present.
fn implicit_runtime_name(pid_path: &Path) -> anyhow::Result<String> {
    let pids = discover_pids(pid_path)?.unwrap_or_default();

    if let [only] = pids.as_slice() {
        return Ok(only.file_stem().to_owned());
    }

    let mut running = pids
        .iter()
        .filter(|pid| matches!(pid.status(), PidStatus::Running(_)));
    if let (Some(only), None) = (running.next(), running.next()) {
        return Ok(only.file_stem().to_owned());
    }

    if pids
        .iter()
        .any(|pid| pid.file_stem() == DEFAULT_RUNTIME_NAME)
    {
        return Ok(String::from(DEFAULT_RUNTIME_NAME));
    }

    if pids.is_empty() {
        anyhow::bail!("no runtimes at {}", pid_path.display());
    }

    let names: Vec<_> = pids.iter().map(Pid::file_stem).collect();
    anyhow::bail!(
        "several runtimes at {} ({}); pass a name",
        pid_path.display(),
        names.join(", ")
    );
}

/// Persisted per-runtime-name state, keyed under the platform data folder.
#[derive(serde::Serialize, serde::Deserialize)]
struct RuntimeIdentity {
    /// The runtime's zenoh id as hex. Stable across restarts: placement
    /// rows, node leases and liveliness tokens all key on it.
    id: String,
}

/// `$XDG_DATA_HOME/myrmic` (or `~/.local/share/myrmic`).
fn data_dir() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("neither XDG_DATA_HOME nor HOME is set")?;
    Ok(base.join("myrmic"))
}

/// `$XDG_DATA_HOME/myrmic/runtimes` (or `~/.local/share/myrmic/runtimes`).
fn identity_dir() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("runtimes"))
}

/// The recorded stable id for a named runtime, or `None` if it never started.
fn load_runtime_id(name: &str) -> anyhow::Result<Option<String>> {
    let path = identity_dir()?.join(format!("{name}.yaml"));

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(anyhow::Error::new(err).context(format!(
                "failed to read runtime identity {}",
                path.display()
            )));
        }
    };

    let identity: RuntimeIdentity = serde_yaml::from_str(&raw)
        .with_context(|| format!("corrupt runtime identity {}", path.display()))?;
    Ok(Some(identity.id))
}

/// The per-runtime folder under the data dir, holding `db`, `logs`, ...
fn runtime_data_dir(id: &str) -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join(id))
}

/// Resolves the runtime a name-scoped command targets to its recorded id,
/// falling back to the implicit runtime when no name was given.
fn resolve_runtime(
    name: Option<String>,
    pid_path: Option<&Path>,
) -> anyhow::Result<(String, String)> {
    let name = match name {
        Some(name) => name,
        None => {
            let default_dir;
            let pid_path = match pid_path {
                Some(path) => path,
                None => {
                    default_dir = default_pid_dir(DEFAULT_PID_DIR);
                    &default_dir
                }
            };
            implicit_runtime_name(pid_path)?
        }
    };

    validate_runtime_name(&name)?;

    let id = load_runtime_id(&name)?.with_context(|| {
        format!("no runtime {name:?} recorded (never started on this machine?)")
    })?;

    Ok((name, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid_file(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(format!("{name}.pid")), contents).expect("write pid file");
    }

    fn own_pid() -> String {
        format!("{}\n", std::process::id())
    }

    #[test]
    fn a_lone_runtime_is_the_implicit_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        pid_file(dir.path(), "solo", "garbage");

        let name = implicit_runtime_name(dir.path()).expect("resolves");
        assert_eq!(name, "solo");
    }

    #[test]
    fn the_only_running_runtime_wins_over_dead_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        pid_file(dir.path(), "alive", &own_pid());
        pid_file(dir.path(), "dead", "garbage");
        pid_file(dir.path(), "gone", "garbage");

        let name = implicit_runtime_name(dir.path()).expect("resolves");
        assert_eq!(name, "alive");
    }

    #[test]
    fn several_dead_runtimes_fall_back_to_default_when_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        pid_file(dir.path(), "default", "garbage");
        pid_file(dir.path(), "other", "garbage");

        let name = implicit_runtime_name(dir.path()).expect("resolves");
        assert_eq!(name, DEFAULT_RUNTIME_NAME);
    }

    #[test]
    fn several_running_runtimes_are_ambiguous() {
        let dir = tempfile::tempdir().expect("tempdir");
        pid_file(dir.path(), "one", &own_pid());
        pid_file(dir.path(), "two", &own_pid());

        let err = implicit_runtime_name(dir.path()).expect_err("ambiguous");
        assert!(err.to_string().contains("pass a name"), "{err}");
    }

    #[test]
    fn no_runtimes_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");

        let err = implicit_runtime_name(dir.path()).expect_err("nothing to target");
        assert!(err.to_string().contains("no runtimes"), "{err}");
    }

    #[test]
    fn conflicting_names_are_rejected() {
        let merged = merge_names(Some(String::from("a")), None).expect("one name passes");
        assert_eq!(merged.as_deref(), Some("a"));

        let merged =
            merge_names(Some(String::from("a")), Some(String::from("a"))).expect("same name");
        assert_eq!(merged.as_deref(), Some("a"));

        merge_names(Some(String::from("a")), Some(String::from("b")))
            .expect_err("two names conflict");
    }
}
