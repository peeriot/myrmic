//! Module implementing the loading of the Wasm binaries

use cell_protocol::{BlobHash, class_blob_id, class_registry_scope};
use db_client::Session;
use db_client::v1::{Client as DbClient, models::blob_resolve};
use sorg_common::{bail, custom_err};
use wasmtime::{Engine, ExternType, Instance, Linker, Module, Store};

use crate::{Result, wasm::cell::state::CellState};

pub(crate) async fn load_module_cell(
    session: &Session,
    class: BlobHash,
    engine: &Engine,
    store: &mut Store<CellState>,
    linker: &Linker<CellState>,
) -> Result<Instance> {
    let binary = load_wasm_binary_from_class(session, class).await?;

    let module = Module::from_binary(engine, &binary)?;
    check_memory_export(&module, &class.to_hex())?;

    let instance = linker
        .instantiate_async(store, &module)
        .await
        .map_err(|err| custom_err!("failed to instantiate module: {err}"))?;
    Ok(instance)
}

pub(crate) async fn load_wasm_binary_from_class(
    session: &Session,
    class: BlobHash,
) -> Result<Vec<u8>> {
    let db = DbClient::new(session);

    let response = db
        .read_tx_in(class_registry_scope(), {
            async move |client, tx| {
                client
                    .send(blob_resolve::Request {
                        id: tx,
                        op: blob_resolve::Op {
                            blob_id: class_blob_id(class),
                            range: None,
                        },
                    })
                    .await
            }
        })
        .await
        .map_err(|err| custom_err!("unable to query class registry: {err}"))?
        .map_err(|err| custom_err!("class registry blob_resolve error: {}", err.message))?;

    let blob = response
        .blob
        .ok_or_else(|| custom_err!("no wasm artifact found for class {}", class.to_hex()))?;

    Ok(blob.blob)
}

fn check_memory_export(module: &Module, label: &str) -> Result<()> {
    if let Some(ExternType::Memory(..)) = module.get_export("memory") {
        Ok(())
    } else {
        bail!(
            "The module binary '{label}' does not export memory and can therefore not be deployed"
        )
    }
}
