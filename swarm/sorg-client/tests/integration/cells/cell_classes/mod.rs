mod add;
mod info;
mod list;
mod remove;
mod resolve;

use cell_protocol::{BlobHash, Sri, class_blob_id, class_registry_scope};
use claims::assert_ok;
use db_client::v1::{Client as DbClient, models::blob_resolve};
use sorg_client::Client as SorgClient;

const CLASS_NAME: &str = "test-cell";
const OTHER_NAME: &str = "other-cell";
const DUMMY_BINARY: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7];
const OTHER_BINARY: &[u8] = &[10, 11, 12, 13, 14, 15];
const AOT_BINARY: &[u8] = &[40, 41, 42];
const INSTANCE_SRI: &str = "test-instance";

fn sorg_client(session: &zenoh::Session) -> SorgClient {
    SorgClient::new(session.clone())
}

/// Seeds a bare instance-registry entry so the "class is in use" checks have an
/// instance referencing the class, without standing up a full swarm.
async fn seed_instance(session: &zenoh::Session, sri: &Sri, class: BlobHash) {
    let record = cell_protocol::CellInstance {
        sri: *sri,
        class,
        gen_id: cell_protocol::Gen::from_parts(1, 1),
        lineage: cell_protocol::SpawnLineage::default(),
    };
    assert_ok!(
        sorg_common::instance_registry::insert_registry_entry(session, &record).await,
        "seeding instance registry entry should succeed"
    );
}

/// The blob of class `id`, read the way runtimes load it: by hash.
async fn class_blob(db: &DbClient, id: BlobHash) -> Option<Vec<u8>> {
    let response = db
        .read_tx_in(class_registry_scope(), async |client, tx_id| {
            Ok(client
                .send(blob_resolve::Request {
                    id: tx_id,
                    op: blob_resolve::Op {
                        blob_id: class_blob_id(id),
                        range: None,
                    },
                })
                .await
                .map_err(|e| format!("{e}"))
                .unwrap()
                .map_err(|e| e.message)
                .unwrap())
        })
        .await
        .unwrap();
    response.blob.map(|b| b.blob)
}
