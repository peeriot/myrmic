use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs programs on the host.
#[derive(Clone)]
pub struct Local;

impl MyrmicBackend for Local {
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
