//! Module with the functionality to configure and deploy components via the swarm tool and use them in tests

use std::process::{Child, Command};
use std::sync::OnceLock;

/// A handle to a process killing the process when the handle is dropped
pub type ScopedProcess = swarm::spawn::Spawned;

/// A private zenoh multicast scouting group for this test *process*.
///
/// We can use how multicast groups work, and just assign a unique subset to
/// each test. They stop stepping on each other's toes, and we can cleanup the
/// nextest config.
fn test_multicast_group() -> &'static str {
    static ADDR: OnceLock<String> = OnceLock::new();
    ADDR.get_or_init(|| {
        // Administratively-scoped multicast (239.0.0.0/8); the low 24 bits of the
        // PID cover the whole PID on Linux, so concurrent processes never collide.
        let pid = std::process::id();
        format!(
            "239.{}.{}.{}:7446",
            (pid >> 16) & 0xFF,
            (pid >> 8) & 0xFF,
            pid & 0xFF
        )
    })
}

/// Points `config` at this test process's private multicast group (see
/// `test_multicast_group`), so a session opened from it stays isolated from
/// other test processes. Use this when you need to keep the rest of a
/// caller-supplied config (e.g. a fixed `ZenohId`); otherwise reach for
/// [`test_session`].
pub fn scope_test_multicast(config: &mut zenoh::Config) {
    config
        .insert_json5(
            "scouting/multicast/address",
            &format!("\"{}\"", test_multicast_group()),
        )
        .expect("failed to scope test multicast group");
}

/// Opens a zenoh **peer** session on this test process's private multicast group
/// (see `test_multicast_group`), so a standalone client or mock can discover
/// swarms started via [`swarm_config!`](crate::swarm_config) while staying isolated from other test
/// processes. Use this instead of `zenoh::open` with a default config: a default
/// session sits on the shared group `224.0.0.224:7446`, so it would miss this
/// test's own (isolated) swarms and collide with other processes' traffic.
pub async fn test_session() -> zenoh::Session {
    let mut config = zenoh::Config::default();
    scope_test_multicast(&mut config);
    zenoh::open(config)
        .await
        .expect("failed to open test zenoh session")
}

/// Sets up a swarm config defined by the provided config file (in the ``tests/data`` dir)
/// returns a handle to the process so that it is killed when we leave the scope
/// of the test
pub async fn set_up_swarm_with_config(config_file: impl AsRef<std::path::Path>) -> ScopedProcess {
    let swarm = swarm::Swarm::from_path(config_file).expect("Unable to configure swarm");
    let mut config = swarm.into_config();
    scope_test_multicast(&mut config.zenoh);
    swarm::Swarm::new(config)
        .wait_in_place()
        .await
        .expect("Unable to spawn swarm")
}

#[macro_export]
macro_rules! crate_path {
    (
        $(
            $segment:literal
        ),+ $(,)?
    ) => {{
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/", $($segment),+))
    }};
    () => {{
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    }};
}

#[macro_export]
macro_rules! swarm_config {
    ($file_name:literal) => {{
        let config_file = $crate::crate_path!("tests/data/", $file_name);
        sorg_tests::set_up_swarm_with_config(config_file).await
    }};
}

// This is my hack for having killable things for the time being - we should go back to using only the swarm methods
// as soon as we have a well-behaved shutdown

/// A handle to a process killing the process when the handle is dropped
pub struct KillableProcess {
    child: Child,
    /// Keeps the generated wrapper config alive for the child's lifetime.
    _config_dir: Option<tempfile::TempDir>,
}

impl KillableProcess {
    fn new(mut command: Command, config_dir: Option<tempfile::TempDir>) -> Self {
        let child = command.spawn().expect("failed to spawn scoped command");
        Self {
            child,
            _config_dir: config_dir,
        }
    }
}

impl Drop for KillableProcess {
    fn drop(&mut self) {
        self.child.kill().unwrap();
        let _ = self.child.wait();
    }
}

/// Sets up a swarm config defined by the provided config file (in the ``tests/data`` dir)
/// returns a handle to the process so that it is killed when we leave the scope
/// of the test
pub async fn set_up_killable_swarm(swarm_file: &str, config_file: &str) -> KillableProcess {
    // The subprocess can't inherit the in-process multicast override.
    // So just work around that by passing the config directly.
    let base = std::path::Path::new(config_file)
        .canonicalize()
        .expect("killable swarm config path must exist");
    let wrapper = format!(
        "local base = import \"{}\";\nbase + {{ zenoh+: {{ scouting+: {{ multicast+: {{ address: \"{}\" }} }} }} }}\n",
        base.display(),
        test_multicast_group(),
    );

    let dir = tempfile::tempdir().expect("failed to create temp config dir");
    let wrapper_path = dir.path().join("swarm_wrapper.jsonnet");
    std::fs::write(&wrapper_path, wrapper).expect("failed to write wrapper config");

    let mut command = Command::new(swarm_file);
    command.arg(&wrapper_path);
    KillableProcess::new(command, Some(dir))
}

#[macro_export]
macro_rules! killable_swarm_config {
    ($file_name: expr) => {{
        let swarm_file = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug/swarm");
        let config_file = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/", $file_name);
        sorg_tests::set_up_killable_swarm(swarm_file, config_file).await
    }};
}
