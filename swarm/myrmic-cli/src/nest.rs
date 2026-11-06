use crate::archive;
use crate::args::Ctx;
use crate::build::{AppInfo, CellClass};
use crate::models::CellInstance;

use sorg_common::{HttpBridgeConfig, MqttBridgeConfig};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A class entry of `classes.json`: its artifacts, named by the content hash
/// each blob is stored under in the archive.
#[derive(Serialize, Deserialize)]
struct NestClass {
    wasm: Option<String>,
    riscv32imac: Option<String>,
}

pub fn write(ctx: &Ctx, path: impl AsRef<Path>, info: AppInfo) -> anyhow::Result<()> {
    let path = path.as_ref();

    let mut entries = vec![];

    // Bake the app name so a deployed nest carries its own grouping name.
    entries.push(archive::Entry::virt("./app-name", info.name));

    // A blob is stored once per content hash, so classes built from the same
    // wasm share an archive entry without sharing a class entry.
    let mut stored = HashSet::new();
    let mut store_blob = |path: Option<PathBuf>| -> anyhow::Result<Option<String>> {
        let Some(path) = path else {
            return Ok(None);
        };
        let bytes = std::fs::read(&path)
            .with_context(|| format!("unable to read artifact: {}", path.display()))?;
        let hash = format!("{:x}", sha2::Sha256::digest(&bytes));
        if stored.insert(hash.clone()) {
            entries.push(archive::Entry::from_path(hash.clone(), path));
        }
        Ok(Some(hash))
    };

    let mut classes = BTreeMap::new();
    for (id, class) in info.classes {
        let CellClass {
            name: _,
            wasm_path,
            riscv32imac,
        } = class;
        let entry = NestClass {
            wasm: store_blob(wasm_path)?,
            riscv32imac: store_blob(riscv32imac)?,
        };
        classes.insert(id, entry);
    }

    for instance in &info.instances {
        if !classes.contains_key(&instance.id) {
            anyhow::bail!(
                "instance '{}' references class '{}', which the app does not define",
                instance.srn.as_deref().unwrap_or(&instance.id),
                instance.id
            );
        }
    }

    if !classes.is_empty() {
        let content =
            serde_json::to_string_pretty(&classes).context("unable to serialise classes")?;
        entries.push(archive::Entry::virt("./classes.json", content));
    }

    if !info.instances.is_empty() {
        let content = serde_json::to_string_pretty(&info.instances)
            .context("unable to serialise instances")?;
        entries.push(archive::Entry::virt("./instances.json", content));
    }

    if !info.mqtt_bridges.is_empty() {
        let bridges = MqttBridgeConfig {
            bridges: info.mqtt_bridges,
        };
        let content =
            serde_json::to_string_pretty(&bridges).context("unable to serialise mqtt bridges")?;
        entries.push(archive::Entry::virt("./mqtt-bridges.json", content));
    }

    if !info.http_bridges.is_empty() {
        let bridges = HttpBridgeConfig {
            api: info.http_bridges,
        };
        let content =
            serde_json::to_string_pretty(&bridges).context("unable to serialise http bridges")?;
        entries.push(archive::Entry::virt("./http-bridges.json", content));
    }

    archive::write(path, &entries)?;

    crate::info!(ctx, "wrote nest archive: {}", path.display());

    Ok(())
}

pub fn read(ctx: &Ctx, path: impl AsRef<Path>) -> anyhow::Result<AppInfo> {
    let path = path.as_ref();
    let file = std::fs::File::open(path)
        .with_context(|| format!("unable to open nest file: {}", path.display()))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);

    let extract_dir = std::env::temp_dir().join(format!("myrmic-nest-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&extract_dir)
        .with_context(|| format!("unable to create temp dir: {}", extract_dir.display()))?;

    let mut classes: BTreeMap<String, NestClass> = BTreeMap::new();
    let mut instances: Vec<CellInstance> = vec![];
    let mut mqtt: Option<MqttBridgeConfig> = None;
    let mut http: Option<HttpBridgeConfig> = None;
    let mut app_name: Option<String> = None;
    let mut hash_paths: HashMap<String, PathBuf> = HashMap::new();

    for entry in tar
        .entries()
        .with_context(|| format!("unable to read nest entries: {}", path.display()))?
    {
        let mut entry = entry.context("invalid nest entry")?;
        let entry_path = entry.path().context("invalid entry path")?.into_owned();
        let name = entry_path.to_string_lossy();
        let name = name.trim_start_matches("./").to_string();

        match name.as_str() {
            "app-name" => app_name = Some(read_text(&mut entry)?.trim().to_string()),
            "classes.json" => {
                classes = serde_json::from_str(&read_text(&mut entry)?)
                    .context("unable to parse classes.json")?;
            }
            "instances.json" => {
                instances = serde_json::from_str(&read_text(&mut entry)?)
                    .context("unable to parse instances.json")?;
            }
            "class-map.json" | "instance-map.json" => anyhow::bail!(
                "nest {} was built by an older myrmic and carries no instance settings; \
                 rebuild it with `myrmic build`",
                path.display()
            ),
            "mqtt-bridges.json" => {
                mqtt = Some(
                    serde_json::from_str(&read_text(&mut entry)?)
                        .context("unable to parse mqtt-bridges.json")?,
                );
            }
            "http-bridges.json" => {
                http = Some(
                    serde_json::from_str(&read_text(&mut entry)?)
                        .context("unable to parse http-bridges.json")?,
                );
            }
            hash => {
                let out = extract_dir.join(hash);
                let mut sink = std::fs::File::create(&out)
                    .with_context(|| format!("unable to create file: {}", out.display()))?;
                std::io::copy(&mut entry, &mut sink)
                    .with_context(|| format!("unable to extract: {}", out.display()))?;
                hash_paths.insert(hash.to_string(), out);
            }
        }
    }

    let classes: HashMap<_, _> = classes
        .into_iter()
        .map(|(id, entry)| {
            let class = CellClass {
                name: id.clone(),
                wasm_path: blob_path(ctx, &hash_paths, &id, "wasm", entry.wasm),
                riscv32imac: blob_path(ctx, &hash_paths, &id, "riscv32imac", entry.riscv32imac),
            };
            (id, class)
        })
        .collect();

    for instance in &instances {
        if !classes.contains_key(&instance.id) {
            anyhow::bail!(
                "instance '{}' references class '{}', which the nest does not contain",
                instance.srn.as_deref().unwrap_or(&instance.id),
                instance.id
            );
        }
    }

    // Legacy nests (built before names were baked) fall back to the nest
    // filename stem. We never invent a name.
    let name = match app_name {
        Some(name) => name,
        None => path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .context("nest has no baked app name and no filename to derive one from")?,
    };

    Ok(AppInfo {
        name,
        instances,
        classes,
        mqtt_bridges: mqtt.map(|c| c.bridges).unwrap_or_default(),
        http_bridges: http.map(|c| c.api).unwrap_or_default(),
    })
}

fn read_text(entry: &mut impl std::io::Read) -> std::io::Result<String> {
    let mut buf = String::new();
    entry.read_to_string(&mut buf)?;
    Ok(buf)
}

/// Where the archive put the blob `hash` of `class`'s `artifact`; `None`, with
/// a warning, when the nest names a blob the archive does not hold.
fn blob_path(
    ctx: &Ctx,
    hash_paths: &HashMap<String, PathBuf>,
    class: &str,
    artifact: &str,
    hash: Option<String>,
) -> Option<PathBuf> {
    let hash = hash?;
    let path = hash_paths.get(&hash).cloned();
    if path.is_none() {
        crate::warn!(
            ctx,
            "nest references {} blob {} for class {} but the archive holds no such entry",
            artifact,
            hash,
            class
        );
    }
    path
}

#[cfg(test)]
mod tests;
