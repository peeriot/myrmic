//! Deploy-time resolution of spawn references.
//!
//! Cells that spawn children embed a placeholder hash per referenced class (via
//! the SDK `declare!` macro). Before upload we resolve each referenced class
//! name to its content hash — from a class built in this same deploy, or from
//! the class registry — and patch the placeholder in place, so the running cell
//! spawns by stable content hash.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, bail};
use myrmic_common::cells::spawn_ref::scan_spawn_refs;
use sha2::Digest as _;

type Hash = [u8; 32];

fn sha256(bytes: &[u8]) -> Hash {
    sha2::Sha256::digest(bytes).into()
}

/// Resolves and patches every spawn reference across a deploy's classes.
///
/// `classes` maps a class name to its Wasm bytes. `registry` resolves a class
/// name not built in this deploy to its already-registered content hash
/// (`None` if unknown). Returns the patched bytes per class. Errors on a
/// reference that resolves nowhere, or on a reference cycle.
pub fn resolve_and_patch(
    classes: BTreeMap<String, Vec<u8>>,
    registry: &mut dyn FnMut(&str) -> anyhow::Result<Option<Hash>>,
) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    let mut patcher = Patcher {
        classes,
        memo: BTreeMap::new(),
        active: BTreeSet::new(),
        registry,
    };
    let names: Vec<String> = patcher.classes.keys().cloned().collect();
    for name in &names {
        patcher.final_hash(name)?;
    }
    Ok(patcher.classes)
}

struct Patcher<'a> {
    classes: BTreeMap<String, Vec<u8>>,
    memo: BTreeMap<String, Hash>,
    active: BTreeSet<String>,
    registry: &'a mut dyn FnMut(&str) -> anyhow::Result<Option<Hash>>,
}

impl Patcher<'_> {
    /// The final content hash of `name` after its own references are patched.
    fn final_hash(&mut self, name: &str) -> anyhow::Result<Hash> {
        if let Some(hash) = self.memo.get(name) {
            return Ok(*hash);
        }
        // A class not built in this deploy is resolved against the registry.
        let Some(bytes) = self.classes.get(name) else {
            return match (self.registry)(name)? {
                Some(hash) => Ok(hash),
                None => bail!(
                    "spawn reference to class '{name}': not built in this deploy \
                     and not found in the class registry"
                ),
            };
        };

        if !self.active.insert(name.to_owned()) {
            bail!("cyclic spawn reference involving class '{name}'");
        }

        // Resolve each referenced child first (its final, patched hash), then
        // patch this class's slots — so a class's hash reflects the exact bytes
        // that will be uploaded.
        let refs = scan_spawn_refs(bytes);
        let mut patches = Vec::with_capacity(refs.len());
        for reference in refs {
            let hash = self.final_hash(&reference.name).with_context(|| {
                format!(
                    "resolving spawn reference '{}' from class '{name}'",
                    reference.name
                )
            })?;
            patches.push((reference.hash_offset, hash));
        }

        let bytes = self.classes.get_mut(name).expect("class present");
        for (offset, hash) in patches {
            bytes[offset..offset + 32].copy_from_slice(&hash);
        }
        let hash = sha256(bytes);

        self.active.remove(name);
        self.memo.insert(name.to_owned(), hash);
        Ok(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use myrmic_common::cells::spawn_ref::SPAWN_REF_MAGIC;

    /// Class body = `prefix` followed by one embedded spawn ref per name
    /// (placeholder hash), matching the on-wire `SpawnRef` layout.
    fn class(prefix: &[u8], refs: &[&str]) -> Vec<u8> {
        let mut v = prefix.to_vec();
        for name in refs {
            v.extend_from_slice(&SPAWN_REF_MAGIC);
            v.extend_from_slice(&[0xAA; 32]);
            v.extend_from_slice(&(name.len() as u32).to_le_bytes());
            v.extend_from_slice(name.as_bytes());
        }
        v
    }

    fn deploy<const N: usize>(entries: [(&str, Vec<u8>); N]) -> BTreeMap<String, Vec<u8>> {
        entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect()
    }

    #[test]
    fn parent_reference_is_patched_with_child_content_hash() {
        let child = class(b"child-body", &[]);
        let parent = class(b"parent-body", &["child"]); // magic@11, hash slot@19
        let child_hash = sha256(&child);

        let mut none = |_: &str| Ok(None);
        let out = resolve_and_patch(
            deploy([("child", child.clone()), ("parent", parent)]),
            &mut none,
        )
        .unwrap();

        assert_eq!(&out["parent"][19..19 + 32], &child_hash);
        assert_eq!(out["child"], child, "leaf class must be untouched");
    }

    #[test]
    fn unknown_reference_is_an_error() {
        let parent = class(b"p", &["ghost"]);
        let mut none = |_: &str| Ok(None);
        let err = resolve_and_patch(deploy([("parent", parent)]), &mut none).unwrap_err();
        assert!(format!("{err:#}").contains("ghost"), "got: {err:#}");
    }

    #[test]
    fn reference_cycle_is_an_error() {
        let a = class(b"a", &["b"]);
        let b = class(b"b", &["a"]);
        let mut none = |_: &str| Ok(None);
        let err = resolve_and_patch(deploy([("a", a), ("b", b)]), &mut none).unwrap_err();
        assert!(format!("{err:#}").contains("cyclic"), "got: {err:#}");
    }

    #[test]
    fn out_of_deploy_reference_resolves_via_registry() {
        let parent = class(b"parent-body", &["remote"]); // hash slot@19
        let remote_hash = [7u8; 32];
        let mut reg = |name: &str| Ok((name == "remote").then_some(remote_hash));

        let out = resolve_and_patch(deploy([("parent", parent)]), &mut reg).unwrap();

        assert_eq!(&out["parent"][19..19 + 32], &remote_hash);
    }
}
