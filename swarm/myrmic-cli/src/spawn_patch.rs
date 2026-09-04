//! Wires deploy-time spawn-reference patching into the app deploy path.
//!
//! The pure resolve/patch logic lives in [`myrmic_build::spawn_patch`]; this
//! module reads the built wasm, prefetches hashes for classes resolved from the
//! registry, runs the patch, and repoints each patched class at a temp file.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Context as _;
use cell_protocol::BlobHash;
use myrmic_common::cells::spawn_ref::scan_spawn_refs;

use crate::args::Ctx;
use crate::build::CellClass;

type Hash = [u8; 32];

/// Resolves and patches every embedded spawn reference across an app's cell
/// classes, before upload. A referenced class is resolved to a content hash —
/// preferring a class built in this same deploy, else the class registry — and
/// the placeholder in the referencing wasm is patched in place. Patched wasm is
/// written to a temp file and the class repointed at it.
pub async fn patch_spawn_refs(
    ctx: Ctx,
    session: &zenoh::Session,
    classes: &mut HashMap<String, CellClass>,
) -> anyhow::Result<()> {
    // Class name -> wasm bytes for every class built in this deploy.
    let mut bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for class in classes.values() {
        if let Some(path) = &class.wasm_path {
            let wasm = std::fs::read(path)
                .with_context(|| format!("unable to read {}", path.display()))?;
            bytes.insert(class.name.clone(), wasm);
        }
    }

    // Collect referenced classes; those not built here must come from the registry.
    let mut external: BTreeSet<String> = BTreeSet::new();
    let mut any_refs = false;
    for wasm in bytes.values() {
        for reference in scan_spawn_refs(wasm) {
            any_refs = true;
            if !bytes.contains_key(&reference.name) {
                external.insert(reference.name);
            }
        }
    }
    if !any_refs {
        return Ok(());
    }

    let mut registry: HashMap<String, Hash> = HashMap::new();
    for name in &external {
        let info = sorg_common::class_registry::get_class_info(session, name)
            .await
            .with_context(|| format!("unable to look up class '{name}' in registry"))?;
        if let Some(BlobHash::Sha2(hash)) = info.and_then(|i| i.wasm_hash) {
            registry.insert(name.clone(), hash);
        }
    }

    let mut resolve =
        |name: &str| -> anyhow::Result<Option<Hash>> { Ok(registry.get(name).copied()) };
    let patched = myrmic_build::spawn_patch::resolve_and_patch(bytes, &mut resolve)?;

    // Repoint each class that embeds references at a temp file holding its
    // patched bytes, so upload/nest hashing sees the resolved hashes.
    let dir = std::env::temp_dir().join(format!("myrmic-spawn-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("unable to create temp dir {}", dir.display()))?;

    for class in classes.values_mut() {
        let Some(new_bytes) = patched.get(&class.name) else {
            continue;
        };
        let count = scan_spawn_refs(new_bytes).len();
        if count == 0 {
            continue;
        }
        let out = dir.join(format!("{}.wasm", class.name));
        std::fs::write(&out, new_bytes)
            .with_context(|| format!("unable to write patched wasm {}", out.display()))?;
        crate::info!(
            ctx,
            "resolved {count} spawn reference(s) in class '{}'",
            class.name
        );
        class.wasm_path = Some(out);
    }

    Ok(())
}
