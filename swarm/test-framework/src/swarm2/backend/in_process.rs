use std::marker::PhantomData;

use enum_map::EnumMap;

use super::SwarmBackend;
use crate::{
    in_process,
    myrmic::{self, Myrmic},
    swarm2::SwarmSpec,
};

/// [`SwarmBackend`] whose runtimes are nodes inside the test process (see [`crate::in_process`]).
///
/// The swarm is operated through an isolated CLI on the host ([`Myrmic::local_isolated`]), like
/// [`super::local::Local`]'s: every node scouts on that CLI's multicast group, so the CLI, the
/// nodes and the test's session find each other, and nothing outside the swarm sees them.
pub struct InProcess<S> {
    /// the CLI that operates the swarm, on the nodes' multicast group
    client: Myrmic<myrmic::Local>,
    /// One builder per runtime, at its [`enum_map::Enum::into_usize`] index. A slice rather than
    /// an `EnumMap`: whether an `EnumMap` is `Send + Sync` depends on `S::Array`, which the
    /// compiler cannot see through in generic code, and [`SwarmBackend`] requires both.
    nodes: Box<[in_process::StoppedRt]>,
    spec: PhantomData<S>,
}

impl<S: SwarmSpec> InProcess<S> {
    /// A backend whose runtime `id` is the node `nodes[id]`, on the swarm's multicast group (which
    /// replaces the builders' own).
    pub fn new(nodes: EnumMap<S, in_process::StoppedRt>) -> Self {
        let client = Myrmic::local_isolated();
        let group = client.backend().get_multicast_group().to_owned();
        Self {
            nodes: nodes
                .into_values()
                .map(|node| node.multicast_group(&group))
                .collect(),
            client,
            spec: PhantomData,
        }
    }
}

impl<S: SwarmSpec> SwarmBackend<S> for InProcess<S> {
    type Runtime = in_process::StoppedRt;

    type Client = myrmic::Local;

    /// a copy of the node's builder, so it starts under the same zid every time
    fn runtime(&self, id: S) -> in_process::StoppedRt {
        self.nodes[id.into_usize()].clone()
    }

    fn client(&self) -> Myrmic<myrmic::Local> {
        self.client.clone()
    }

    async fn connect_session(&self) -> zenoh::Session {
        zenoh::open(self.client.backend().zenoh_config())
            .await
            .expect("failed to open zenoh session")
    }
}
