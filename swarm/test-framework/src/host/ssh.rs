use std::{net::IpAddr, sync::Arc};

use super::{Id, Remote, Router, Shared};
use crate::{
    myrmic::{self, Myrmic},
    overlay,
};

/// A machine the test reaches over SSH, e.g. a Raspberry Pi of the rack. It exists on its own,
/// before any host set: host sets take it in, and give it its address there. Clones are the same
/// host.
#[derive(Clone)]
pub struct Ssh {
    /// SSH destination, e.g. `user@rack-node-1.example`: anything `ssh` accepts (host aliases from
    /// `~/.ssh/config` included); it also names the host in messages
    destination: Arc<str>,
    /// the directory the machine's `myrmic` and `zenoh-router` lie in (the test puts them there,
    /// see [`crate::ssh_utils::upload`])
    bin_dir: Arc<str>,
    shared: Arc<Shared>,
}

impl Ssh {
    /// The machine at the SSH destination `destination`, with its `myrmic` and `zenoh-router` in
    /// `bin_dir`, nothing in its firewall and in no host set yet.
    pub fn new(destination: impl Into<String>, bin_dir: impl Into<String>) -> Self {
        Self {
            destination: Arc::from(destination.into()),
            bin_dir: Arc::from(bin_dir.into()),
            shared: Arc::default(),
        }
    }

    /// its identity
    pub(crate) fn get_id(&self) -> Id {
        Id {
            shared: Arc::downgrade(&self.shared),
            label: self.destination.clone(),
        }
    }

    /// record its address on the host set named `host_set`, which takes it in
    pub(crate) fn set_address(&self, host_set: &str, address: IpAddr) {
        self.shared
            .lock_addresses()
            .insert(host_set.to_owned(), address);
    }

    /// its SSH destination
    pub(crate) fn get_destination(&self) -> &str {
        &self.destination
    }

    /// the directory its binaries lie in
    pub(crate) fn get_bin_dir(&self) -> &str {
        &self.bin_dir
    }

    /// The CLI on this machine, attached by `attachment`. Like [`super::Local::myrmic_isolated`],
    /// it has state directories of its own, created on the machine here and removed there with the
    /// last clone of the shim, so its runtimes see no name, identity or data of an earlier run.
    pub fn myrmic_isolated(
        &self,
        attachment: impl Into<overlay::Attachment>,
    ) -> Myrmic<myrmic::Ssh> {
        Myrmic::new(
            myrmic::Ssh::isolated(self.clone()),
            format!("{}/myrmic", self.bin_dir),
            attachment.into(),
        )
    }

    /// A zenoh router on this machine, a new member of an overlay attached by `attachment`.
    /// Returns once it listens, so a swarm can attach through it right away (see [`Router`]).
    pub async fn router(&self, attachment: impl Into<overlay::Attachment>) -> Router<Self> {
        let member = attachment.into().add_member(self.get_id());
        let binary = format!("{}/zenoh-router", self.bin_dir);
        Router::start(self.clone(), &binary, member).await
    }

    /// `ssh <options> <destination> <words>`, the words quoted so the remote shell runs them as
    /// they are
    pub(crate) fn ssh(&self, options: &[&str], words: &[&str]) -> std::process::Command {
        let mut command = crate::ssh_utils::command_with_ssh_config("ssh");
        command
            .args(options)
            .arg(&*self.destination)
            .arg(crate::ssh_utils::quoted_command_line(words));
        command
    }
}

impl Remote for Ssh {
    /// `ssh [-i identity] [-o UserKnownHostsFile=..] [-o StrictHostKeyChecking=..] <destination>
    /// <quoted program args>`
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut words = vec![program];
        words.extend(args);
        self.ssh(&[], &words)
    }
}
