use std::{
    net::IpAddr,
    sync::{Arc, LazyLock},
};

use super::{Id, Shared};
use crate::{
    World,
    myrmic::{self, Myrmic},
    overlay,
};

/// what others know of the test's own machine, which lives as long as the process
static LOCAL_SHARED: LazyLock<Arc<Shared>> = LazyLock::new(Arc::default);

/// the identity of the test's own machine ([`Local`])
pub(crate) fn get_local_id() -> Id {
    Id {
        shared: Arc::downgrade(&LOCAL_SHARED),
        label: Arc::from("the test's machine"),
    }
}

/// record the address the test's own machine has on the network named `network`, which it just
/// joined
pub(crate) fn set_local_address(network: &str, address: IpAddr) {
    LOCAL_SHARED
        .lock_addresses()
        .insert(network.to_owned(), address);
}

/// forget the address the test's own machine had on the network named `network`, which is gone
pub(crate) fn remove_local_address(network: &str) {
    let removed = LOCAL_SHARED.lock_addresses().remove(network);
    assert!(
        removed.is_some(),
        "the test's machine has no address on network `{network}`"
    );
}

/// The test's own machine.
pub struct Local;

impl Local {
    /// The CLI on this machine, attached by `attachment`, running the locally built `myrmic` binary
    /// (see [`crate::resolve_binary!`]). The CLI keeps its state in directories of its own (runtime
    /// identities, databases, pid files, logs), which are removed with the last clone of the shim.
    /// A runtime's identity is its name within those directories. So tests that give their runtimes
    /// the same names can run in parallel, and nothing piles up in the machine's own directories.
    pub fn myrmic_isolated(
        &self,
        attachment: impl Into<overlay::Attachment>,
    ) -> Myrmic<myrmic::Local> {
        Myrmic::new(myrmic::Local::isolated(), local_binary(), attachment.into())
    }

    /// Like [`Self::myrmic_isolated`], but on a group of its own, in an overlay of its own on
    /// [`World::local`]. For a test whose runtimes all run on this machine and that never needs the
    /// group itself.
    pub fn myrmic_isolated_alone(&self) -> Myrmic<myrmic::Local> {
        let group = overlay::Overlay::new(&World::local()).new_multicast_group();
        self.myrmic_isolated(&group)
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
