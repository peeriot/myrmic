use cell_protocol::{
    AddMode, ArtifactInfo, ArtifactLocation, ArtifactPlatform, CLASS_REGISTRY_TABLE, ClassArtifact,
    ClassInfo, class_registry_scope,
};
use db_client::v1::{
    Client as DbClient,
    models::{
        BlobHash, TxId, blob_link, blob_store, blob_unlink, tb_delete, tb_get, tb_insert, tb_list,
    },
};
use zenoh::Session;

use super::instance_registry;
use crate::{Result, bail, custom_err};

async fn bail_if_instances_exist(client: &DbClient, tx_id: TxId, name: &str) -> Result<()> {
    let instances = instance_registry::do_list(client, tx_id).await?;
    if instances.iter().any(|i| i.class_name == name) {
        bail!(
            "class '{}' has active instances and cannot be modified",
            name
        );
    }
    Ok(())
}

pub async fn list_classes(session: &Session) -> Result<Vec<ClassInfo>> {
    let db = DbClient::new(session);

    db.read_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_list(client, tx_id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

pub async fn add_class_artifact(
    session: &Session,
    name: &str,
    artifact: ClassArtifact,
    mode: AddMode,
) -> Result<ClassInfo> {
    let name = name.to_owned();
    let db = DbClient::new(session);

    db.write_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_add_artifact(client, tx_id, &name, artifact, mode).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

pub async fn get_class_info(session: &Session, name: &str) -> Result<Option<ClassInfo>> {
    let name = name.to_owned();
    let db = DbClient::new(session);

    db.read_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_get(client, tx_id, &name).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

pub async fn get_class_info_in_tx(
    client: &DbClient,
    tx_id: TxId,
    name: &str,
) -> Result<Option<ClassInfo>> {
    do_get(client, tx_id, name).await
}

async fn do_add_artifact(
    client: &DbClient,
    tx_id: TxId,
    name: &str,
    artifact: ClassArtifact,
    mode: AddMode,
) -> Result<ClassInfo> {
    match artifact {
        ClassArtifact::Wasm(binary) => do_add_wasm(client, tx_id, name, &binary, mode).await,
        ClassArtifact::Aot {
            platform: target,
            aot_blob,
            meta_blob,
        } => do_add_aot(client, tx_id, name, target, &aot_blob, &meta_blob, mode).await,
    }
}

async fn do_add_wasm(
    client: &DbClient,
    tx_id: TxId,
    name: &str,
    binary: &[u8],
    mode: AddMode,
) -> Result<ClassInfo> {
    let hash = BlobHash::of(binary);
    let existing = do_get(client, tx_id, name).await?;
    let all_classes = do_list(client, tx_id).await?;
    let hash_owner = all_classes
        .iter()
        .find(|c| c.wasm_hash.as_ref() == Some(&hash));

    if let Some(existing_info) = &existing {
        if existing_info.wasm_hash.as_ref() == Some(&hash) {
            tracing::info!("class '{}' already has this wasm binary", name);
            return Ok(existing_info.clone());
        }

        if existing_info.wasm_hash.is_some() {
            match mode {
                AddMode::Strict => {
                    bail!(
                        "class '{}' already exists with a different wasm binary",
                        name
                    );
                }
                AddMode::Force => {
                    bail_if_instances_exist(client, tx_id, name).await?;
                    if let Some(owner) = hash_owner
                        && owner.name != name
                    {
                        bail!(
                            "cannot overwrite class '{}': wasm binary already registered under class '{}'",
                            name,
                            owner.name
                        );
                    }
                    tracing::warn!("overwriting wasm binary for class '{}'", name);
                    do_unlink_blob(client, tx_id, ArtifactLocation::wasm(name)).await?;
                }
            }
        }

        do_resolve_hash_conflict(client, tx_id, name, hash_owner, mode).await?;
        do_store_blob(client, tx_id, ArtifactLocation::wasm(name), binary).await?;
        let info = ClassInfo {
            name: name.to_owned(),
            wasm_hash: Some(hash),
            artifacts: existing_info.artifacts.clone(),
        };
        do_delete_table_entry(client, tx_id, name).await?;
        do_insert_table_entry(client, tx_id, &info).await?;
        return Ok(info);
    }

    do_resolve_hash_conflict(client, tx_id, name, hash_owner, mode).await?;
    do_store_blob(client, tx_id, ArtifactLocation::wasm(name), binary).await?;
    let info = ClassInfo {
        name: name.to_owned(),
        wasm_hash: Some(hash),
        artifacts: vec![],
    };
    do_insert_table_entry(client, tx_id, &info).await?;
    Ok(info)
}

async fn do_resolve_hash_conflict(
    client: &DbClient,
    tx_id: TxId,
    name: &str,
    hash_owner: Option<&ClassInfo>,
    mode: AddMode,
) -> Result<()> {
    let Some(owner) = hash_owner else {
        return Ok(());
    };
    match mode {
        AddMode::Strict => {
            bail!(
                "wasm binary already registered under class '{}'",
                owner.name
            );
        }
        AddMode::Force => {
            tracing::warn!(
                "reassigning wasm binary from class '{}' to '{}'",
                owner.name,
                name
            );
            do_unlink_all_blobs(client, tx_id, owner).await?;
            do_delete_table_entry(client, tx_id, &owner.name).await?;
        }
    }
    Ok(())
}

async fn do_add_aot(
    client: &DbClient,
    tx_id: TxId,
    name: &str,
    target: ArtifactPlatform,
    aot_blob: &[u8],
    meta_blob: &[u8],
    mode: AddMode,
) -> Result<ClassInfo> {
    let aot_hash = BlobHash::of(aot_blob);
    let meta_hash = BlobHash::of(meta_blob);
    let existing = do_get(client, tx_id, name).await?;
    let is_new = existing.is_none();

    let mut info = existing.unwrap_or_else(|| ClassInfo {
        name: name.to_owned(),
        wasm_hash: None,
        artifacts: vec![],
    });

    if let Some(existing_artifact) = info.artifacts.iter().find(|a| a.platform == target) {
        if existing_artifact.aot_hash == aot_hash && existing_artifact.meta_hash == meta_hash {
            tracing::info!(
                "class '{}' already has identical artifacts for target '{}'",
                name,
                target
            );
            return Ok(info);
        }

        match mode {
            AddMode::Strict => {
                bail!(
                    "class '{}' already has different artifacts for target '{}'",
                    name,
                    target
                );
            }
            AddMode::Force => {
                bail_if_instances_exist(client, tx_id, name).await?;
                tracing::warn!(
                    "overwriting artifacts for target '{}' on class '{}'",
                    target,
                    name
                );
                do_unlink_blob(client, tx_id, ArtifactLocation::aot(name, target)).await?;
                do_unlink_blob(client, tx_id, ArtifactLocation::meta(name, target)).await?;
                info.artifacts.retain(|a| a.platform != target);
            }
        }
    }

    do_store_blob(client, tx_id, ArtifactLocation::aot(name, target), aot_blob).await?;
    do_store_blob(
        client,
        tx_id,
        ArtifactLocation::meta(name, target),
        meta_blob,
    )
    .await?;

    info.artifacts.push(ArtifactInfo {
        platform: target,
        aot_hash,
        meta_hash,
    });

    if !is_new {
        do_delete_table_entry(client, tx_id, name).await?;
    }
    do_insert_table_entry(client, tx_id, &info).await?;

    Ok(info)
}

pub async fn remove_class(session: &Session, name: &str) -> Result<()> {
    let name = name.to_owned();
    let db = DbClient::new(session);

    db.write_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_remove_class(client, tx_id, &name).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

async fn do_remove_class(client: &DbClient, tx_id: TxId, name: &str) -> Result<()> {
    let existing = do_get(client, tx_id, name).await?;
    let Some(info) = existing else {
        bail!("class '{}' not found", name);
    };

    bail_if_instances_exist(client, tx_id, name).await?;
    do_unlink_all_blobs(client, tx_id, &info).await?;
    do_delete_table_entry(client, tx_id, name).await
}

async fn do_unlink_all_blobs(client: &DbClient, tx_id: TxId, info: &ClassInfo) -> Result<()> {
    if info.wasm_hash.is_some() {
        do_unlink_blob(client, tx_id, ArtifactLocation::wasm(&info.name)).await?;
    }
    for artifact in &info.artifacts {
        do_unlink_blob(
            client,
            tx_id,
            ArtifactLocation::aot(&info.name, artifact.platform),
        )
        .await?;
        do_unlink_blob(
            client,
            tx_id,
            ArtifactLocation::meta(&info.name, artifact.platform),
        )
        .await?;
    }
    Ok(())
}

async fn do_list(client: &DbClient, tx_id: TxId) -> Result<Vec<ClassInfo>> {
    let response = client
        .send(tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope: class_registry_scope(),
                table: CLASS_REGISTRY_TABLE.to_owned(),
                cursor: None,
                limit: None,
                order: None,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send list request: {}", err))?
        .map_err(|err| custom_err!("unable to list classes: {}", err.message))?;

    response
        .entities
        .into_iter()
        .map(|(_id, value)| {
            postcard::from_bytes(&value)
                .map_err(|_| custom_err!("failed to deserialize class entry"))
        })
        .collect()
}

pub(crate) async fn do_get(
    client: &DbClient,
    tx_id: TxId,
    name: &str,
) -> Result<Option<ClassInfo>> {
    let response = client
        .send(tb_get::Request {
            id: tx_id,
            op: tb_get::Op {
                scope: class_registry_scope(),
                table: CLASS_REGISTRY_TABLE.to_owned(),
                eid: name.as_bytes().to_vec(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send get request: {}", err))?
        .map_err(|err| custom_err!("unable to get class: {}", err.message))?;

    match response.value {
        Some(bytes) => {
            let info = postcard::from_bytes(&bytes)
                .map_err(|_| custom_err!("failed to deserialize class entry"))?;
            Ok(Some(info))
        }
        None => Ok(None),
    }
}

async fn do_insert_table_entry(client: &DbClient, tx_id: TxId, info: &ClassInfo) -> Result<()> {
    let value =
        postcard::to_allocvec(info).map_err(|_| custom_err!("failed to serialize class entry"))?;

    client
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: class_registry_scope(),
                table: CLASS_REGISTRY_TABLE.to_owned(),
                eid: Some(info.name.as_bytes().to_vec()),
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send insert request: {}", err))?
        .map_err(|err| custom_err!("unable to insert class: {}", err.message))?;

    Ok(())
}

async fn do_unlink_blob(client: &DbClient, tx_id: TxId, location: ArtifactLocation) -> Result<()> {
    let (scope, path) = location.into_parts();
    client
        .send(blob_unlink::Request {
            id: tx_id,
            op: blob_unlink::Op { scope, path },
        })
        .await
        .map_err(|err| custom_err!("unable to send unlink request: {}", err))?
        .map_err(|err| custom_err!("unable to unlink blob: {}", err.message))?;

    Ok(())
}

async fn do_delete_table_entry(client: &DbClient, tx_id: TxId, name: &str) -> Result<()> {
    client
        .send(tb_delete::Request {
            id: tx_id,
            op: tb_delete::Op {
                scope: class_registry_scope(),
                table: CLASS_REGISTRY_TABLE.to_owned(),
                eid: name.as_bytes().to_vec(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send delete request: {}", err))?
        .map_err(|err| custom_err!("unable to delete class: {}", err.message))?;

    Ok(())
}

async fn do_store_blob(
    client: &DbClient,
    tx_id: TxId,
    location: ArtifactLocation,
    binary: &[u8],
) -> Result<()> {
    let (scope, path) = location.into_parts();
    let blob_id = client
        .send(blob_store::Request {
            id: tx_id,
            op: blob_store::Op {
                scope,
                blob: binary.to_vec(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send store request: {}", err))?
        .map_err(|err| custom_err!("unable to store blob: {}", err.message))?
        .blob_id;

    client
        .send(blob_link::Request {
            id: tx_id,
            op: blob_link::Op { blob_id, path },
        })
        .await
        .map_err(|err| custom_err!("unable to send link request: {}", err))?
        .map_err(|err| custom_err!("unable to link blob: {}", err.message))?;

    Ok(())
}
