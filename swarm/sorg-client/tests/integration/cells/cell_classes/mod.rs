mod add;
mod info;
mod list;
mod remove;
mod resolve;

use cell_protocol::{ArtifactLocation, ArtifactPlatform, ClassArtifact, Sri};
use claims::assert_ok;
use db_client::v1::{Client as DbClient, models::path_resolve};
use sorg_client::Client as SorgClient;

const CLASS_NAME: &str = "test-cell";
const DUMMY_BINARY: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7];
const INSTANCE_SRI: &str = "test-instance";

fn sorg_client(session: &zenoh::Session) -> SorgClient {
    SorgClient::new(session.clone())
}

/// Seeds a bare instance-registry entry so the "class is in use" checks have an
/// instance referencing the class, without standing up a full swarm.
async fn seed_instance(session: &zenoh::Session, sri: &Sri, class_name: &str) {
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

async fn blob_at_path(db: &DbClient, location: ArtifactLocation) -> Option<Vec<u8>> {
    let (scope, path) = location.into_parts();
    let response = db
        .read_tx_in(scope.clone(), async |client, tx_id| {
            Ok(client
                .send(path_resolve::Request {
                    id: tx_id,
                    op: path_resolve::Op {
                        scope,
                        path,
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

fn wasm(binary: &[u8]) -> ClassArtifact {
    ClassArtifact::Wasm(binary.to_vec())
}

fn aot(target: ArtifactPlatform, aot_blob: &[u8], meta_blob: &[u8]) -> ClassArtifact {
    ClassArtifact::Aot {
        platform: target,
        aot_blob: aot_blob.to_vec(),
        meta_blob: meta_blob.to_vec(),
    }
}
