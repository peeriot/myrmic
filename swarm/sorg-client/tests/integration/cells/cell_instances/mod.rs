mod erase;
mod inspect;
mod list;

use cell_protocol::Sri;
use claims::assert_ok;
use sorg_client::Client as SorgClient;

pub const INSTANCE_SRI: &str = "test-instance";
pub const CLASS_NAME: &str = "test-class";

pub fn sorg_client(session: &zenoh::Session) -> SorgClient {
    SorgClient::new(session.clone())
}

/// Seeds a bare instance-registry entry the way a deployment would, so tests that only
/// need an instance to *exist* stay lightweight (no full swarm).
pub async fn seed_instance(session: &zenoh::Session, sri: &Sri, class_name: &str) {
    let record = cell_protocol::CellInstance {
        sri: *sri,
        class_name: class_name.to_owned(),
        gen_id: cell_protocol::Gen::from_parts(1, 1),
        lineage: cell_protocol::SpawnLineage::default(),
    };
    assert_ok!(
        sorg_common::instance_registry::insert_registry_entry(session, &record).await,
        "seeding instance registry entry should succeed"
    );
}
