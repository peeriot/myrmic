use sorg_common::{bail, custom_err};
use tracing::debug;
use zenoh::config::ZenohId;

use crate::Result;

pub(crate) struct OrchCluster {
    own_id: ZenohId,
    known_orchs: Vec<ZenohId>,
    leader: ZenohId,
}

impl OrchCluster {
    pub(super) fn new(own_id: ZenohId) -> Self {
        let known_orchs = vec![own_id];
        let leader =
            get_leader(&known_orchs).expect("always have at least the node itself at this point");
        if leader == own_id {
            debug!(orch_lead = "elected", node_id = %own_id, "Node {own_id} became leader");
        }
        Self {
            own_id,
            known_orchs,
            leader,
        }
    }

    pub(super) fn add_member(&mut self, zid: ZenohId) -> Result<()> {
        if self.own_id == zid {
            bail!("own id {zid} being added as new member");
        }
        if self.known_orchs.contains(&zid) {
            debug!("orch {zid} already known, skipping");
            return Ok(());
        }

        if zid < self.leader {
            if zid == self.own_id {
                debug!(orch_lead = "elected", node_id = %zid, "Node {zid} became leader");
            } else if self.leader == self.own_id {
                debug!(orch_lead = "resigned", node_id = %self.own_id, "Node resigned leadership");
            }
            self.leader = zid;
        } else {
            debug!("{lead} remains leader", lead = self.leader);
        }
        self.known_orchs.push(zid);
        Ok(())
    }

    pub(super) fn remove_member(&mut self, zid: ZenohId) -> Result<()> {
        debug_assert!(self.own_id != zid, "removing ourselves");
        if let Some(pos) = self.known_orchs.iter().position(|id| id == &zid) {
            self.known_orchs.swap_remove(pos);
            let new_leader = get_leader(&self.known_orchs)?;
            if self.leader != new_leader {
                self.leader = new_leader;
                if self.leader == self.own_id {
                    debug!(orch_lead = "elected", node_id = %self.own_id, "Node {new_leader} became leader");
                }
            }
            Ok(())
        } else {
            debug!("leaving node with id {zid} does not host an orch");
            Ok(())
        }
    }

    pub(super) fn is_leader(&self) -> bool {
        self.own_id == self.leader
    }

    pub(super) fn contains_member(&self, zid: ZenohId) -> bool {
        self.known_orchs.contains(&zid)
    }
}

fn get_leader(known_orchs: &[ZenohId]) -> Result<ZenohId> {
    let leader = *known_orchs
        .iter()
        .min()
        .ok_or(custom_err!("no orch nodes for leader selection"))?;
    Ok(leader)
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use zenoh::config::ZenohId;

    use super::OrchCluster;

    fn zid_one() -> ZenohId {
        ZenohId::from_str("37c72f467bc9c77f41b73fe16f054741").unwrap()
    }
    fn zid_two() -> ZenohId {
        ZenohId::from_str("2cc8a35064c529faaa1924134d13e2ad").unwrap()
    }
    fn zid_three() -> ZenohId {
        ZenohId::from_str("1619e204bc90ec6fa7870dac7842dac5").unwrap()
    }

    #[test]
    fn orch_cluster() {
        let own_id = zid_two();

        let mut tested = OrchCluster::new(own_id);
        tested.add_member(zid_one()).unwrap();
        tested.add_member(zid_three()).unwrap();

        assert!(!tested.is_leader());

        tested.remove_member(zid_one()).unwrap();
        assert!(tested.is_leader());

        tested.remove_member(zid_three()).unwrap();
        assert!(tested.is_leader());

        tested.add_member(zid_three()).unwrap();
        assert!(tested.is_leader());

        tested.add_member(zid_one()).unwrap();
        assert!(!tested.is_leader());
    }
}
