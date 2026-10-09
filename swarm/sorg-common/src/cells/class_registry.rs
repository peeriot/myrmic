//! The cell class registry. A class is identified by the content hash of its
//! artifact: a wasm class by its wasm, an AOT entry by its AOT binary.
//! Everything that references a class holds that id. Names are labels for
//! people: a separate table maps a name to an id, several names may point to
//! one class, and binding a name to another class only moves the label.
//!
//! Classes are removed manually only: unbind a name, then remove the class if
//! nothing references it any more (see [`ClassReference`]). Their blobs stay:
//! a db delete only adds a tombstone, so erasing them would free nothing.

use std::collections::{BTreeMap, HashMap};

use cell_protocol::{
    ArtifactPlatform, BlobHash, CLASS_NAMES_TABLE, CLASS_REGISTRY_TABLE, CellInstance, ClassInfo,
    Sri, class_registry_key, class_registry_scope,
};
use db_client::v1::{
    Client as DbClient,
    models::{TxId, blob_store, tb_delete, tb_get, tb_insert, tb_list},
};
use myrmic_common::cells::spawn_ref::scan_spawn_refs;
use zenoh::Session;

use super::{instance_registry, root_restart};
use crate::{CellConfig, CellDeployment, Result, bail, custom_err};

/// Something that keeps a class from being removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassReference {
    /// A name bound to the class.
    Name(String),
    /// A cell instance created from the class.
    Instance(Sri),
    /// A root's restart spec, which deploys the class again.
    RestartSpec(Sri),
    /// A wasm class compiled to this AOT entry, or spawning this class.
    Class(BlobHash),
}

/// The outcome of removing a class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// The class and the AOT entries only it pointed to are gone.
    Removed,
    /// The class stays: these still reference it.
    Kept(Vec<ClassReference>),
}

/// Registers a wasm class and its AOT compilations, and binds `name` to it,
/// all in one transaction. Idempotent: the same wasm yields the same class, and
/// an AOT replaces the one the class had for that platform. Returns the class
/// id.
pub async fn add_class(
    session: &Session,
    name: &str,
    wasm: Vec<u8>,
    aots: Vec<(ArtifactPlatform, Vec<u8>)>,
) -> Result<BlobHash> {
    let name = name.to_owned();
    let db = DbClient::new(session);

    db.write_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_add_class(client, tx_id, &name, wasm, aots).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Adds an AOT binary under `name`: compiled from the wasm class the name
/// points to, if it points to one; otherwise as a class of its own, moving the
/// name to it. Returns the AOT's id.
pub async fn add_aot(
    session: &Session,
    name: &str,
    platform: ArtifactPlatform,
    aot: Vec<u8>,
) -> Result<BlobHash> {
    let name = name.to_owned();
    let db = DbClient::new(session);

    db.write_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_add_aot_by_name(client, tx_id, &name, platform, aot).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// The class `name` points to, if it is bound.
pub async fn resolve_name(session: &Session, name: &str) -> Result<Option<BlobHash>> {
    let name = name.to_owned();
    let db = DbClient::new(session);

    db.read_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_resolve(client, tx_id, &name).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Every bound name with the class it points to.
pub async fn list_names(session: &Session) -> Result<Vec<(String, BlobHash)>> {
    let db = DbClient::new(session);

    db.read_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_list_names(client, tx_id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Every registered class with its id.
pub async fn list_classes(session: &Session) -> Result<Vec<(BlobHash, ClassInfo)>> {
    let db = DbClient::new(session);

    db.read_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_list_classes(client, tx_id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

pub async fn get_class_info(session: &Session, id: BlobHash) -> Result<Option<ClassInfo>> {
    let db = DbClient::new(session);

    db.read_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_get(client, tx_id, id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Reads several classes in one transaction routed to the class registry's own
/// scope, so a batch costs one locate however many ids it carries. The ids are
/// sorted and deduplicated first: a batch naming the same class twice pays a
/// single point read, and the op order is reproducible for the same request.
/// A class with no row is absent from the result rather than an error.
pub async fn get_class_infos(
    session: &Session,
    ids: &[BlobHash],
) -> Result<HashMap<BlobHash, ClassInfo>> {
    let mut ids = ids.to_vec();
    ids.sort_unstable_by_key(|id| id.to_hex());
    ids.dedup();
    let db = DbClient::new(session);

    db.read_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_get_many(client, tx_id, &ids).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// The AOT binary that runs `class` on `platform`, with its CRC: the class's
/// compilation for that platform if it is a wasm class, the class itself if it
/// is an AOT-only class for that platform. `None` if there is none.
pub async fn aot_for(
    session: &Session,
    class: BlobHash,
    platform: ArtifactPlatform,
) -> Result<Option<(BlobHash, u32)>> {
    let db = DbClient::new(session);

    db.read_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_aot_for(client, tx_id, class, platform).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Unbinds `name` if it points to `id`. Returns whether it did. Removing a
/// class means unbinding its name with this, then [`remove_if_unreferenced`].
pub async fn unbind_name(session: &Session, name: &str, id: BlobHash) -> Result<bool> {
    let name = name.to_owned();
    let db = DbClient::new(session);

    db.write_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_unbind_if(client, tx_id, &name, id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Removes the class `id`, and the AOT entries only it pointed to, unless
/// something references it.
///
/// Instances and restart specs are read in transactions routed to their own
/// scopes; the registry's own rows are checked again in the removing
/// transaction. A deploy racing this on another node can still reference a
/// class this removes: removal is a manual operation, and that is accepted.
pub async fn remove_if_unreferenced(session: &Session, id: BlobHash) -> Result<Removal> {
    let instances = instance_registry::list_instances(session).await?;
    let specs = root_restart::list_specs(session).await?;
    let db = DbClient::new(session);

    db.write_tx_in(class_registry_scope(), async move |client, tx_id| {
        Ok(do_remove_if_unreferenced(client, tx_id, id, &instances, &specs).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

async fn do_add_class(
    client: &DbClient,
    tx_id: TxId,
    name: &str,
    wasm: Vec<u8>,
    aots: Vec<(ArtifactPlatform, Vec<u8>)>,
) -> Result<BlobHash> {
    let spawns = spawned_classes(&wasm);
    let id = do_store_blob(client, tx_id, wasm).await?;

    let mut aot = match do_get(client, tx_id, id).await? {
        None => BTreeMap::new(),
        Some(ClassInfo::Wasm { aot, .. }) => aot,
        Some(ClassInfo::Aot { .. }) => {
            bail!(
                "binary {} is already registered as an AOT binary, not as wasm",
                id.to_hex()
            )
        }
    };
    for (platform, binary) in aots {
        let aot_id = do_add_aot(client, tx_id, platform, binary).await?;
        aot.insert(platform, aot_id);
    }

    do_put(client, tx_id, id, &ClassInfo::Wasm { aot, spawns }).await?;
    do_bind(client, tx_id, name, id).await?;
    Ok(id)
}

async fn do_add_aot_by_name(
    client: &DbClient,
    tx_id: TxId,
    name: &str,
    platform: ArtifactPlatform,
    binary: Vec<u8>,
) -> Result<BlobHash> {
    let aot_id = do_add_aot(client, tx_id, platform, binary).await?;

    let wasm = match do_resolve(client, tx_id, name).await? {
        Some(id) => match do_get(client, tx_id, id).await? {
            Some(ClassInfo::Wasm { aot, spawns }) => Some((id, aot, spawns)),
            Some(ClassInfo::Aot { .. }) => None,
            None => bail!(
                "name '{name}' points to class {}, which is not registered",
                id.to_hex()
            ),
        },
        None => None,
    };

    match wasm {
        Some((id, mut aot, spawns)) => {
            aot.insert(platform, aot_id);
            do_put(client, tx_id, id, &ClassInfo::Wasm { aot, spawns }).await?;
        }
        // Not a wasm class to compile from: the AOT is a class of its own.
        None => do_bind(client, tx_id, name, aot_id).await?,
    }
    Ok(aot_id)
}

/// Stores an AOT binary and its entry. Returns its id.
async fn do_add_aot(
    client: &DbClient,
    tx_id: TxId,
    platform: ArtifactPlatform,
    binary: Vec<u8>,
) -> Result<BlobHash> {
    let crc = wasm_storage::metadata::CRC.checksum(&binary);
    let id = do_store_blob(client, tx_id, binary).await?;
    match do_get(client, tx_id, id).await? {
        None => do_put(client, tx_id, id, &ClassInfo::Aot { platform, crc }).await?,
        Some(ClassInfo::Aot {
            platform: stored, ..
        }) if stored == platform => {}
        Some(other) => bail!(
            "binary {} is already registered as {other:?}, not as an AOT binary for {platform}",
            id.to_hex()
        ),
    }
    Ok(id)
}

async fn do_aot_for(
    client: &DbClient,
    tx_id: TxId,
    class: BlobHash,
    platform: ArtifactPlatform,
) -> Result<Option<(BlobHash, u32)>> {
    match do_get(client, tx_id, class).await? {
        Some(ClassInfo::Wasm { aot, .. }) => match aot.get(&platform) {
            Some(&aot_id) => match do_get(client, tx_id, aot_id).await? {
                Some(ClassInfo::Aot { crc, .. }) => Ok(Some((aot_id, crc))),
                // A wasm entry and the AOT entries it points to are written in
                // one transaction, and an AOT entry is only removed once no wasm
                // entry points to it.
                other => bail!(
                    "class {} points to AOT {} for {platform}, which is {other:?}",
                    class.to_hex(),
                    aot_id.to_hex()
                ),
            },
            None => Ok(None),
        },
        Some(ClassInfo::Aot { platform: own, crc }) if own == platform => Ok(Some((class, crc))),
        Some(ClassInfo::Aot { .. }) | None => Ok(None),
    }
}

async fn do_remove_if_unreferenced(
    client: &DbClient,
    tx_id: TxId,
    id: BlobHash,
    instances: &[CellInstance],
    specs: &[CellDeployment],
) -> Result<Removal> {
    let names = do_list_names(client, tx_id).await?;
    let classes = do_list_classes(client, tx_id).await?;

    let Some((_, info)) = classes.iter().find(|(class, _)| *class == id) else {
        bail!("class {} not found", id.to_hex());
    };

    let refs = references(id, &names, &classes, instances, specs);
    if refs.is_empty() {
        do_delete(client, tx_id, id).await?;

        // The AOT entries this class pointed to go with it, unless something
        // else references them too: another wasm class, a name, a cell.
        if let ClassInfo::Wasm { aot, .. } = info {
            let remaining: Vec<(BlobHash, ClassInfo)> = classes
                .iter()
                .filter(|(class, _)| *class != id)
                .cloned()
                .collect();
            for &aot_id in aot.values() {
                if references(aot_id, &names, &remaining, instances, specs).is_empty() {
                    do_delete(client, tx_id, aot_id).await?;
                }
            }
        }
        Ok(Removal::Removed)
    } else {
        Ok(Removal::Kept(refs))
    }
}

/// Everything that references class `id`.
fn references(
    id: BlobHash,
    names: &[(String, BlobHash)],
    classes: &[(BlobHash, ClassInfo)],
    instances: &[CellInstance],
    specs: &[CellDeployment],
) -> Vec<ClassReference> {
    let names = names
        .iter()
        .filter(|(_, class)| *class == id)
        .map(|(name, _)| ClassReference::Name(name.clone()));
    let instances = instances
        .iter()
        .filter(|instance| instance.class == id)
        .map(|instance| ClassReference::Instance(instance.sri));
    let specs = specs
        .iter()
        .filter(|spec| matches!(spec.config, CellConfig::Wasm { class } if class == id))
        .map(|spec| ClassReference::RestartSpec(spec.sri));
    let classes = classes
        .iter()
        .filter(|(_, info)| match info {
            ClassInfo::Wasm { aot, spawns } => {
                aot.values().any(|a| *a == id) || spawns.contains(&id)
            }
            ClassInfo::Aot { .. } => false,
        })
        .map(|(class, _)| ClassReference::Class(*class));

    names.chain(instances).chain(specs).chain(classes).collect()
}

/// The child classes `wasm` spawns: the hashes patched into its spawn
/// references at deploy. An unpatched reference yields the placeholder, which
/// is no class, so it references nothing.
fn spawned_classes(wasm: &[u8]) -> Vec<BlobHash> {
    scan_spawn_refs(wasm)
        .into_iter()
        .map(|found| {
            let slot = &wasm[found.hash_offset..found.hash_offset + 32];
            BlobHash::Sha2(slot.try_into().expect("a hash slot is 32 bytes"))
        })
        .collect()
}

async fn do_store_blob(client: &DbClient, tx_id: TxId, binary: Vec<u8>) -> Result<BlobHash> {
    let blob_id = client
        .send(blob_store::Request {
            id: tx_id,
            op: blob_store::Op {
                scope: class_registry_scope(),
                blob: binary,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send store request: {}", err))?
        .map_err(|err| custom_err!("unable to store blob: {}", err.message))?
        .blob_id;

    Ok(blob_id.hash)
}

async fn do_get(client: &DbClient, tx_id: TxId, id: BlobHash) -> Result<Option<ClassInfo>> {
    let response = client
        .send(tb_get::Request {
            id: tx_id,
            op: tb_get::Op {
                scope: class_registry_scope(),
                table: CLASS_REGISTRY_TABLE.to_owned(),
                eid: class_registry_key(id),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send get request: {}", err))?
        .map_err(|err| custom_err!("unable to get class: {}", err.message))?;

    response
        .value
        .map(|bytes| {
            postcard::from_bytes(&bytes)
                .map_err(|_| custom_err!("failed to deserialize class entry"))
        })
        .transpose()
}

async fn do_put(client: &DbClient, tx_id: TxId, id: BlobHash, info: &ClassInfo) -> Result<()> {
    let value =
        postcard::to_allocvec(info).map_err(|_| custom_err!("failed to serialize class entry"))?;

    client
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: class_registry_scope(),
                table: CLASS_REGISTRY_TABLE.to_owned(),
                eid: Some(class_registry_key(id)),
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send insert request: {}", err))?
        .map_err(|err| custom_err!("unable to insert class: {}", err.message))?;

    Ok(())
}

async fn do_delete(client: &DbClient, tx_id: TxId, id: BlobHash) -> Result<()> {
    client
        .send(tb_delete::Request {
            id: tx_id,
            op: tb_delete::Op {
                scope: class_registry_scope(),
                table: CLASS_REGISTRY_TABLE.to_owned(),
                eid: class_registry_key(id),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send delete request: {}", err))?
        .map_err(|err| custom_err!("unable to delete class: {}", err.message))?;

    Ok(())
}

async fn do_list_classes(client: &DbClient, tx_id: TxId) -> Result<Vec<(BlobHash, ClassInfo)>> {
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
        .map(|(key, value)| {
            let id = std::str::from_utf8(&key)
                .ok()
                .and_then(BlobHash::from_hex)
                .ok_or_else(|| custom_err!("malformed class key"))?;
            let info = postcard::from_bytes(&value)
                .map_err(|_| custom_err!("failed to deserialize class entry"))?;
            Ok((id, info))
        })
        .collect()
}

async fn do_resolve(client: &DbClient, tx_id: TxId, name: &str) -> Result<Option<BlobHash>> {
    let response = client
        .send(tb_get::Request {
            id: tx_id,
            op: tb_get::Op {
                scope: class_registry_scope(),
                table: CLASS_NAMES_TABLE.to_owned(),
                eid: name.as_bytes().to_vec(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send get request: {}", err))?
        .map_err(|err| custom_err!("unable to get class name: {}", err.message))?;

    response
        .value
        .map(|bytes| {
            postcard::from_bytes(&bytes)
                .map_err(|_| custom_err!("failed to deserialize class name entry"))
        })
        .transpose()
}

async fn do_bind(client: &DbClient, tx_id: TxId, name: &str, id: BlobHash) -> Result<()> {
    let value = postcard::to_allocvec(&id)
        .map_err(|_| custom_err!("failed to serialize class name entry"))?;

    client
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: class_registry_scope(),
                table: CLASS_NAMES_TABLE.to_owned(),
                eid: Some(name.as_bytes().to_vec()),
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send insert request: {}", err))?
        .map_err(|err| custom_err!("unable to bind class name: {}", err.message))?;

    Ok(())
}

async fn do_get_many(
    client: &DbClient,
    tx_id: TxId,
    ids: &[BlobHash],
) -> Result<HashMap<BlobHash, ClassInfo>> {
    let mut infos = HashMap::with_capacity(ids.len());
    for &id in ids {
        if let Some(info) = do_get(client, tx_id, id).await? {
            infos.insert(id, info);
        }
    }
    Ok(infos)
}

async fn do_unbind_if(client: &DbClient, tx_id: TxId, name: &str, id: BlobHash) -> Result<bool> {
    match do_resolve(client, tx_id, name).await? {
        Some(bound) if bound == id => {
            do_unbind(client, tx_id, name).await?;
            Ok(true)
        }
        // Bound to another class, or not at all: neither is `id`'s label.
        Some(_) | None => Ok(false),
    }
}

async fn do_unbind(client: &DbClient, tx_id: TxId, name: &str) -> Result<()> {
    client
        .send(tb_delete::Request {
            id: tx_id,
            op: tb_delete::Op {
                scope: class_registry_scope(),
                table: CLASS_NAMES_TABLE.to_owned(),
                eid: name.as_bytes().to_vec(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send delete request: {}", err))?
        .map_err(|err| custom_err!("unable to unbind class name: {}", err.message))?;

    Ok(())
}

async fn do_list_names(client: &DbClient, tx_id: TxId) -> Result<Vec<(String, BlobHash)>> {
    let response = client
        .send(tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope: class_registry_scope(),
                table: CLASS_NAMES_TABLE.to_owned(),
                cursor: None,
                limit: None,
                order: None,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send list request: {}", err))?
        .map_err(|err| custom_err!("unable to list class names: {}", err.message))?;

    response
        .entities
        .into_iter()
        .map(|(key, value)| {
            let name =
                String::from_utf8(key).map_err(|_| custom_err!("malformed class name key"))?;
            let id = postcard::from_bytes(&value)
                .map_err(|_| custom_err!("failed to deserialize class name entry"))?;
            Ok((name, id))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use cell_protocol::{Gen, SpawnLineage};
    use myrmic_common::cells::spawn_ref::SpawnRef;

    use super::*;

    fn id(n: u8) -> BlobHash {
        BlobHash::Sha2([n; 32])
    }

    fn sri(name: &str) -> Sri {
        Sri::of_path(name).unwrap()
    }

    fn instance(name: &str, class: BlobHash) -> CellInstance {
        CellInstance {
            sri: sri(name),
            class,
            gen_id: Gen::from_parts(1, 1),
            lineage: SpawnLineage::default(),
        }
    }

    fn spec(name: &str, class: BlobHash) -> CellDeployment {
        CellDeployment::new(sri(name), CellConfig::Wasm { class })
    }

    fn wasm(aot: &[(ArtifactPlatform, BlobHash)], spawns: &[BlobHash]) -> ClassInfo {
        ClassInfo::Wasm {
            aot: aot.iter().copied().collect(),
            spawns: spawns.to_vec(),
        }
    }

    #[test]
    fn an_unreferenced_class_has_no_references() {
        let classes = [(id(1), wasm(&[], &[]))];
        assert_eq!(references(id(1), &[], &classes, &[], &[]), vec![]);
    }

    #[test]
    fn every_kind_of_reference_is_reported() {
        let names = [("a".to_owned(), id(1)), ("other".to_owned(), id(9))];
        let classes = [
            (id(1), wasm(&[], &[])),
            (id(2), wasm(&[], &[id(1)])),
            (id(3), wasm(&[(ArtifactPlatform::Riscv32imac, id(1))], &[])),
        ];
        let instances = [instance("cell", id(1)), instance("unrelated", id(9))];
        let specs = [spec("root", id(1))];

        assert_eq!(
            references(id(1), &names, &classes, &instances, &specs),
            vec![
                ClassReference::Name("a".to_owned()),
                ClassReference::Instance(sri("cell")),
                ClassReference::RestartSpec(sri("root")),
                ClassReference::Class(id(2)),
                ClassReference::Class(id(3)),
            ]
        );
    }

    #[test]
    fn spawned_classes_are_the_patched_hashes() {
        let child = SpawnRef::<{ "child".len() }>::new("child");
        let mut wasm = b"before".to_vec();
        // SAFETY: `SpawnRef` is `#[repr(C)]` plain data; this reads its bytes.
        wasm.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                (&raw const child).cast::<u8>(),
                size_of::<SpawnRef<{ "child".len() }>>(),
            )
        });
        let slot = scan_spawn_refs(&wasm)[0].hash_offset;
        wasm[slot..slot + 32].copy_from_slice(&[7; 32]);

        assert_eq!(spawned_classes(&wasm), vec![id(7)]);
    }
}
