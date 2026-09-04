use std::path::Path;

use filestore_client::Client;
use swarm::spawn::Spawned;

pub async fn load_into_db(file_path: &str, db_swarm: &Spawned) {
    let fs_client = Client::new(db_swarm.session());
    let bytes = std::fs::read(file_path).expect("failed to read file");
    let file_name = Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .expect("failed to figure out file name while loading into db");
    let fs_name = format!("/{file_name}");
    fs_client
        .store_file(&fs_name, bytes)
        .await
        .expect("failed to store file in db");
}
