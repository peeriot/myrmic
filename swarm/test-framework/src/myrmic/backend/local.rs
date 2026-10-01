use std::path::PathBuf;

use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs a myrmic binary on the host.
#[derive(Clone)]
pub struct LocalBinary {
    binary: String,
}

impl LocalBinary {
    /// wrap the myrmic binary at `binary`
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        let binary = binary.into();
        Self {
            binary: binary
                .to_str()
                .unwrap_or_else(|| panic!("myrmic binary path {} is not UTF-8", binary.display()))
                .to_owned(),
        }
    }
}

impl MyrmicBackend for LocalBinary {
    fn binary(&self) -> &str {
        &self.binary
    }

    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(program);
        command.args(args);
        command
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // there is no wrapper: a program that cannot be started is a spawn error
        &[]
    }
}
