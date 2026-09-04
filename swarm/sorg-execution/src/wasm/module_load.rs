//! Module implementing the loading of the Wasm binaries

use cell_protocol::ArtifactLocation;
use db_client::Session;
use db_client::v1::{Client as DbClient, models::path_resolve};
use sorg_common::{bail, custom_err};
use wasmtime::{Engine, ExternType, Instance, Linker, Module, Store};

use crate::{Result, wasm::cell::state::CellState};

pub(crate) async fn load_module_cell(
    session: &Session,
    class_name: &str,
    engine: &Engine,
    store: &mut Store<CellState>,
    linker: &Linker<CellState>,
) -> Result<Instance> {
    let binary = load_wasm_binary_from_class(session, class_name).await?;

    let module = Module::from_binary(engine, &binary)?;
    check_memory_export(&module, class_name)?;

    let instance = linker
        .instantiate_async(store, &module)
        .await
        .map_err(|err| custom_err!("failed to instantiate module: {err}"))?;
    Ok(instance)
}

pub(crate) async fn load_wasm_binary_from_class(
    session: &Session,
    class_name: &str,
) -> Result<Vec<u8>> {
    let db = DbClient::new(session);
    let (scope, path) = ArtifactLocation::wasm(class_name).into_parts();

    let response = db
        .read_tx_in(scope.clone(), {
            async move |client, tx| {
                client
                    .send(path_resolve::Request {
                        id: tx,
                        op: path_resolve::Op {
                            scope,
                            path,
                            range: None,
                        },
                    })
                    .await
            }
        })
        .await
        .map_err(|err| custom_err!("unable to query class registry: {err}"))?
        .map_err(|err| custom_err!("class registry path_resolve error: {}", err.message))?;

    let blob = response
        .blob
        .ok_or_else(|| custom_err!("no wasm artifact found for class '{class_name}'"))?;

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
