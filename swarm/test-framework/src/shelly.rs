//! Shelly smart plugs (Gen2 and later), switched through their RPC API, e.g. to cut the power of
//! the hosts plugged into one.
//!
//! A plug answers over HTTPS with a certificate that isn't checked, and wants HTTP digest
//! authentication as the user `admin`. `curl` does both, so the requests go through it.

use std::io::Write;
use std::process::{Command, Stdio};

/// How long a plug may take to answer.
const REQUEST_TIMEOUT_SECS: &str = "10";

/// Switch the plug at `address` (a host name or IP address) on or off, logging in with
/// `password`. Panics if the plug doesn't confirm it.
pub fn switch(address: &str, password: &str, on: bool) {
    let url = format!("https://{address}/rpc/Switch.Set?id=0&on={on}");
    // the password goes in on stdin, as `curl` config: on the command line, every process on the
    // machine could read it
    let mut curl = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--insecure",
            "--digest",
        ])
        .args(["--max-time", REQUEST_TIMEOUT_SECS, "--config", "-", &url])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to run curl: {e}"));
    let escaped = password.replace('\\', r"\\").replace('"', r#"\""#);
    curl.stdin
        .take()
        .expect("curl's stdin is piped")
        .write_all(format!("user = \"admin:{escaped}\"\n").as_bytes())
        .unwrap_or_else(|e| panic!("failed to pass the login to curl: {e}"));
    let output = curl
        .wait_with_output()
        .unwrap_or_else(|e| panic!("failed to wait for curl: {e}"));
    let state = if on { "on" } else { "off" };
    assert!(
        output.status.success(),
        "failed to switch the plug at {address} {state}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
