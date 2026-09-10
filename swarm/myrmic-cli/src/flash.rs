//! Flashing a firmware onto an attached board and watching it run.
//!
//! The connect/assemble/write routine lives in `myrmic-flash`, so that it can be shared with other
//! tools.

use std::io::{Read as _, Write as _};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use espflash::cli::monitor::parser::ResolvingPrinter;

pub use myrmic_flash::{Board, config, espflash_chip, picked};

use crate::args::Ctx;

const BAUD: u32 = 115_200;

/// How long the board's port may stay away before the monitor gives up. A
/// native USB-Serial-JTAG re-enumerates on every reset, so the port vanishes
/// briefly right after flashing and on every reboot after that.
const REAPPEAR_TIMEOUT: Duration = Duration::from_secs(10);

/// Streams the board's serial output to stdout until the process is
/// interrupted, reopening the port whenever it drops. Addresses in a panic
/// backtrace are resolved against `elfs`: the firmware, and the chip's ROM
/// when espflash ships it.
pub fn monitor(ctx: &Ctx, port_name: &str, elfs: Vec<&[u8]>) -> anyhow::Result<()> {
    crate::info!(ctx, "Monitoring {port_name} (Ctrl-C to stop)");
    let mut out = ResolvingPrinter::new(elfs, std::io::stdout().lock(), false);
    let mut buf = [0u8; 1024];
    let mut gone_since: Option<Instant> = None;

    loop {
        let mut port = match serialport::new(port_name, BAUD)
            .timeout(Duration::from_millis(100))
            .open()
        {
            Ok(port) => port,
            Err(e) => {
                let since = *gone_since.get_or_insert_with(Instant::now);
                if since.elapsed() > REAPPEAR_TIMEOUT {
                    return Err(e).with_context(|| {
                        format!("{port_name} did not come back within {REAPPEAR_TIMEOUT:?}")
                    });
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        gone_since = None;

        loop {
            match port.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.write_all(&buf[..n])?;
                    out.flush()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => break,
            }
        }
        crate::debug!(ctx, "{port_name} went away; waiting for it to return");
    }
}
