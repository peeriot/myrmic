//! The machines a test's runtimes run on: the test's own machine ([`Local`]), a remote machine
//! over SSH ([`Ssh`]), a docker container ([`Docker`]). A host hands out the CLI shims that run
//! on it ([`crate::myrmic::Myrmic`]), and through them the runtimes.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{host, overlay::Overlay};
//!
//! let group = Overlay::new().new_multicast_group();
//! let myrmic = host::Local.myrmic_isolated(&group);  // the CLI on the test's machine
//! let pi = host::Ssh::new("pi@rack-1", "myrmic");
//! let remote = pi.myrmic(&group);  // the CLI on the pi
//! ```
//!
//! # Design notes
//!
//! A host is not the same as a shim's backend ([`crate::myrmic::MyrmicBackend`]): the backend
//! only knows how to run a command there, while a host is the machine, which carries shims (one
//! per group or isolation) and, in time, its address and its faults.

use std::sync::Arc;

use crate::{
    docker::container::ManagedContainer,
    myrmic::{self, Myrmic},
    overlay::MulticastGroup,
};

/// The test's own machine.
pub struct Local;

impl Local {
    /// The CLI on this machine, on `group`, running the locally built myrmic binary (see
    /// [`crate::resolve_binary!`]). It has state directories of its own (runtime identities,
    /// databases, pid files, logs), removed with the last clone of the shim: a runtime's identity
    /// is its name within them, so tests that give their runtimes the same names can run in
    /// parallel, and nothing piles up in the machine's own directories.
    pub fn myrmic_isolated(&self, group: &MulticastGroup) -> Myrmic<myrmic::Local> {
        Myrmic::new(myrmic::Local::isolated(), local_binary(), group)
    }
}

/// A remote machine, reached over SSH.
pub struct Ssh {
    backend: myrmic::Ssh,
    /// the myrmic binary on the machine, a path or a name on the remote user's `PATH`
    binary: String,
}

impl Ssh {
    /// The machine `login` (anything `ssh` accepts, e.g. `user@rack-node-1.example`), whose myrmic
    /// binary is `binary`: a path (e.g. where a benchmark harness `scp`'d it) or `myrmic` for the
    /// one on the remote user's `PATH`.
    pub fn new(login: impl Into<String>, binary: impl Into<String>) -> Self {
        Self {
            backend: myrmic::Ssh::new(login),
            binary: binary.into(),
        }
    }

    /// the CLI on this machine, on its own state directories and on `group`
    pub fn myrmic(&self, group: &MulticastGroup) -> Myrmic<myrmic::Ssh> {
        Myrmic::new(self.backend.clone(), self.binary.clone(), group)
    }
}

/// A docker container, which has `myrmic` on its `PATH`.
pub struct Docker {
    backend: myrmic::Docker,
}

impl Docker {
    /// The container `container`; it is removed once the last host or shim using it drops.
    pub fn new(container: Arc<ManagedContainer>) -> Self {
        Self {
            backend: myrmic::Docker::attach(container),
        }
    }

    /// the CLI in this container, on `group`
    pub fn myrmic(&self, group: &MulticastGroup) -> Myrmic<myrmic::Docker> {
        Myrmic::new(self.backend.clone(), "myrmic".to_owned(), group)
    }
}

/// the locally built myrmic binary (see [`crate::resolve_binary!`])
fn local_binary() -> String {
    let binary = crate::resolve_binary!("myrmic");
    binary
        .to_str()
        .unwrap_or_else(|| panic!("myrmic binary path {} is not UTF-8", binary.display()))
        .to_owned()
}
