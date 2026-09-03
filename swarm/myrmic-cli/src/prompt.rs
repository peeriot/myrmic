//! Interactive prompts, on stderr with the rest of the CLI's chatter.

use std::io::ErrorKind;

use dialoguer::Select;
use dialoguer::console::Term;

/// Runs `prompt` and returns the chosen index, or `None` when the user backs
/// out with Escape, `q` or Ctrl+C.
///
/// dialoguer hides the cursor while the list is up and shows it again only on
/// the exits it knows about. Ctrl+C is not one of them: console raises SIGINT
/// from inside the key read, and the process dies with the cursor still hidden.
/// Ignoring SIGINT for the prompt's duration turns that keypress into an
/// `Interrupted` read instead, and the cursor is restored on every exit.
pub fn select(prompt: Select<'_>) -> anyhow::Result<Option<usize>> {
    let term = Term::stderr();
    let result = {
        let _sigint = IgnoredSigint::install();
        prompt.interact_on_opt(&term)
    };
    let _ = term.show_cursor();
    let _ = term.flush();
    match result {
        Ok(choice) => Ok(choice),
        Err(dialoguer::Error::IO(err)) if err.kind() == ErrorKind::Interrupted => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// SIGINT ignored until dropped, when the previous disposition is restored.
struct IgnoredSigint(libc::sigaction);

impl IgnoredSigint {
    fn install() -> Self {
        // SAFETY: an all-zero `sigaction` is a valid value (empty mask, no
        // flags), and both pointers refer to live locals.
        unsafe {
            let mut ignore: libc::sigaction = std::mem::zeroed();
            ignore.sa_sigaction = libc::SIG_IGN;
            let mut previous: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGINT, &raw const ignore, &raw mut previous);
            Self(previous)
        }
    }
}

impl Drop for IgnoredSigint {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the disposition `sigaction` itself reported.
        unsafe {
            libc::sigaction(libc::SIGINT, &raw const self.0, std::ptr::null_mut());
        }
    }
}
