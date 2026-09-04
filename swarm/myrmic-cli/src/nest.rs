use crate::archive;
use crate::args::Ctx;
use crate::build::{AppInfo, CellClass};
use crate::models::CellInstance;

use sorg_common::{HttpBridgeConfig, MqttBridgeConfig};

use anyhow::Context as _;
use sha2::Digest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn write(ctx: Ctx, path: impl AsRef<Path>, info: AppInfo) -> anyhow::Result<()> {
    let path = path.as_ref();

    let mut entries = vec![];

    // Bake the app name so a deployed nest carries its own grouping name.
    entries.push(archive::Entry::virt("./app-name", info.name));

    let mut class_map = HashMap::<String, String>::new();

    if !info.classes.is_empty() {
        for (id, class) in info.classes {
            let CellClass {
                name: _,
                wasm_path: a_wasm_path,
                riscv32imac: a_esp32_c6,
            } = class;

            macro_rules! store_blob {
                ($path:expr) => {{
                    if let Some(path) = $path {
                        let bytes = std::fs::read(&path).with_context(|| {
                            format!("unable to read artifact: {}", path.display())
                        })?;
                        let hash = format!("{:x}", sha2::Sha256::digest(&bytes));

                        entries.push(archive::Entry::from_path(hash.clone(), path));
                        Some(hash)
                    } else {
                        None
                    }
                }};
            }

            // the decision on which wasm blob to use should lie with the sorg layer.
            // because we don't have a way to inform the sorg which one to pick, we're just ignoring the esp stuff for now.
            // _but_ we're still supporting the original linux blob path, because that's supported by sorg.
            let hash = store_blob!(a_wasm_path);
            let (aot, meta) = a_esp32_c6.unzip();
            let _hash = store_blob!(aot);
            let _hash = store_blob!(meta);

            if let Some(hash) = hash {
                class_map.insert(id, hash);
            }
        }

        {
            let content = serde_json::to_string_pretty(&class_map)
                .context("unable to serialise class map")?;

            entries.push(archive::Entry::virt("./class-map.json", content));
        }
    }

    if !info.instances.is_empty() {
        let mut instance_map = HashMap::<String, String>::new();

        for instance in info.instances {
            let Some(hash) = class_map.get(&instance.id) else {
                anyhow::bail!("unable to locate class with {}", instance.id);
            };

            let srn = instance.srn.unwrap_or(instance.id);
            instance_map.insert(srn, hash.clone());
        }

        let content = serde_json::to_string_pretty(&instance_map)
            .context("unable to serialise instance map")?;

        entries.push(archive::Entry::virt("./instance-map.json", content));
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

#[allow(clippy::too_many_lines)]
pub fn read(ctx: Ctx, path: impl AsRef<Path>) -> anyhow::Result<AppInfo> {
    use std::io::Read;

    let path = path.as_ref();
    let file = std::fs::File::open(path)
        .with_context(|| format!("unable to open nest file: {}", path.display()))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);

    let extract_dir = std::env::temp_dir().join(format!("myrmic-nest-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&extract_dir)
        .with_context(|| format!("unable to create temp dir: {}", extract_dir.display()))?;

    let mut class_map: HashMap<String, String> = HashMap::new();
    let mut instance_map: HashMap<String, String> = HashMap::new();
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
            "app-name" => {
                let mut buf = String::new();
                entry.read_to_string(&mut buf)?;
                app_name = Some(buf.trim().to_string());
            }
            "class-map.json" => {
                let mut buf = String::new();
                entry.read_to_string(&mut buf)?;
                class_map = serde_json::from_str(&buf).context("unable to parse class-map.json")?;
            }
            "instance-map.json" => {
                let mut buf = String::new();
                entry.read_to_string(&mut buf)?;
                instance_map =
                    serde_json::from_str(&buf).context("unable to parse instance-map.json")?;
            }
            "mqtt-bridges.json" => {
                let mut buf = String::new();
                entry.read_to_string(&mut buf)?;
                mqtt =
                    Some(serde_json::from_str(&buf).context("unable to parse mqtt-bridges.json")?);
            }
            "http-bridges.json" => {
                let mut buf = String::new();
                entry.read_to_string(&mut buf)?;
                http =
                    Some(serde_json::from_str(&buf).context("unable to parse http-bridges.json")?);
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

    // class_map gives us id -> hash; reconstruct one CellClass per id.
    let mut classes = HashMap::with_capacity(class_map.len());
    for (id, hash) in &class_map {
        let wasm_path = hash_paths.get(hash).cloned();
        if wasm_path.is_none() {
            crate::warn!(
                ctx,
                "nest references hash {} for class {} but no matching blob in archive",
                hash,
                id
            );
        }
        classes.insert(
            id.clone(),
            CellClass {
                name: id.clone(),
                wasm_path,
                riscv32imac: None,
            },
        );
    }

    // Each instance_map entry was written as (sri, hash); reverse hash -> class id.
    let hash_to_class: HashMap<&String, &String> =
        class_map.iter().map(|(id, hash)| (hash, id)).collect();

    let mut instances = Vec::with_capacity(instance_map.len());
    for (sri, hash) in instance_map {
        let class_id = hash_to_class
            .get(&hash)
            .map(|id| (*id).clone())
            .with_context(|| format!("no class entry references hash {}", hash))?;
        instances.push(CellInstance {
            id: class_id,
            srn: Some(sri),
            tags: vec![],
            // The nest instance-map is a flat sri->hash mapping; like `tags`,
            // per-instance init arguments and restart policy are not carried
            // through a nest.
            arguments: None,
            restart: None,
        });
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
