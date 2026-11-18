use std::process::Output;
use std::time::{Duration, Instant};

/// How long one attempt may take to connect: a host that is still booting doesn't answer at all.
const CONNECT_TIMEOUT_SECS: &str = "5";

/// How long to wait between two attempts.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Wait until the host `login` (anything `ssh` accepts) takes SSH connections, e.g. after it
/// rebooted. Panics after `timeout`.
pub fn wait_until_reachable(login: &str, timeout: Duration) {
    poll(
        login,
        &["true"],
        timeout,
        "to take SSH connections",
        |output| output.status.success(),
    );
}

/// Wait until the host `login` has synchronized its wall clock (systemd's `NTPSynchronized`), e.g.
/// after it booted without a clock that kept the time. Panics after `timeout`.
pub fn wait_until_clock_synchronized(login: &str, timeout: Duration) {
    let show = [
        "timedatectl",
        "show",
        "--property",
        "NTPSynchronized",
        "--value",
    ];
    poll(
        login,
        &show,
        timeout,
        "to synchronize its clock",
        |output| output.status.success() && output.stdout.trim_ascii() == b"yes",
    );
}

/// Run `words` on the host `login` until `done` says its output is the awaited one. Panics after
/// `timeout`, saying what the host failed `to` do and showing the last output.
fn poll(login: &str, words: &[&str], timeout: Duration, to: &str, done: impl Fn(&Output) -> bool) {
    let start = Instant::now();
    let mut output = run(login, words);
    while !done(&output) {
        assert!(
            start.elapsed() < timeout,
            "{login} failed {to} within {timeout:?}; last: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::thread::sleep(POLL_INTERVAL);
        output = run(login, words);
    }
}

/// `words` run on the host `login` over SSH
fn run(login: &str, words: &[&str]) -> Output {
    super::command_with_ssh_config("ssh")
        .args(["-o", "BatchMode=yes", "-o"])
        .arg(format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}"))
        .arg(login)
        .arg(super::quoted_command_line(words))
        .output()
        .unwrap_or_else(|e| panic!("failed to run ssh: {e}"))
}
