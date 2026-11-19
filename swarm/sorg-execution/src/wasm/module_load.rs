//! Module implementing the loading of the Wasm binaries

use cell_protocol::{BlobHash, class_blob_id, class_registry_scope};
use db_client::Session;
use db_client::v1::{Client as DbClient, models::blob_resolve};
use sorg_common::{bail, custom_err};
use wasmtime::{
    Engine, ExternType, Instance, Linker, Module, Store, StoreLimits, StoreLimitsBuilder,
};

use crate::{Result, wasm::cell::state::CellState};

pub(crate) async fn load_module_cell(
    session: &Session,
    class: BlobHash,
    engine: &Engine,
    store: &mut Store<CellState>,
    linker: &Linker<CellState>,
    max_memory: u64,
) -> Result<Instance> {
    let binary = load_wasm_binary_from_class(session, class).await?;

    let module = Module::from_binary(engine, &binary)?;
    check_memory_export(&module, &class.to_hex(), max_memory)?;

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

pub(crate) fn cell_store_limits(max_memory: u64) -> Result<StoreLimits> {
    let max_memory = usize::try_from(max_memory).map_err(|err| {
        custom_err!(
            "execution.cell_memory ({max_memory}) does not fit this platform's address space: {err}"
        )
    })?;

    Ok(StoreLimitsBuilder::new()
        .memory_size(max_memory)
        .memories(1)
        .build())
}

fn check_memory_export(module: &Module, label: &str, max_memory: u64) -> Result<()> {
    let Some(ExternType::Memory(memory)) = module.get_export("memory") else {
        bail!(
            "The module binary '{label}' does not export memory and can therefore not be deployed"
        )
    };
    let initial = memory.minimum().saturating_mul(memory.page_size());
    if initial > max_memory {
        bail!(
            "The module binary '{label}' declares {initial} bytes of initial memory, above this node's limit execution.cell_memory = {max_memory} bytes"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: u64 = 65_536;

    #[test]
    fn initial_memory_above_limit_is_refused() -> Result<()> {
        let module = module(r#"(module (memory (export "memory") 2))"#)?;

        let err = check_memory_export(&module, "label", LIMIT)
            .expect_err("128 KiB initial memory must not fit 64 KiB")
            .to_string();

        assert_eq!(
            err,
            "The module binary 'label' declares 131072 bytes of initial memory, above this node's limit execution.cell_memory = 65536 bytes"
        );

        Ok(())
    }

    #[test]
    fn initial_memory_within_limit_is_accepted() -> Result<()> {
        let module = module(r#"(module (memory (export "memory") 1))"#)?;

        check_memory_export(&module, "label", LIMIT)
    }

    #[test]
    fn memory_grow_past_limit_fails() -> Result<()> {
        assert_eq!(grow_one_page(LIMIT)?, -1);

        Ok(())
    }

    #[test]
    fn memory_grow_within_limit_returns_old_size() -> Result<()> {
        assert_eq!(grow_one_page(2 * LIMIT)?, 1);

        Ok(())
    }

    #[test]
    fn second_memory_is_refused_even_within_the_size_limit() -> Result<()> {
        let module = module(r#"(module (memory 1) (memory (export "memory") 1))"#)?;
        check_memory_export(&module, "label", LIMIT)?;
        let mut store = limited_store(&module, LIMIT)?;

        let err = Instance::new(&mut store, &module, &[])
            .expect_err("a second memory must be refused")
            .to_string();

        assert!(err.contains("memory count too high"), "{err}");

        Ok(())
    }

    fn module(wat: &str) -> Result<Module> {
        Ok(Module::new(&Engine::default(), wat)?)
    }

    fn limited_store(module: &Module, limit: u64) -> Result<Store<StoreLimits>> {
        let mut store = Store::new(module.engine(), cell_store_limits(limit)?);
        store.limiter(|limits| limits);

        Ok(store)
    }

    fn grow_one_page(limit: u64) -> Result<i32> {
        let module = module(
            r#"(module
                (memory (export "memory") 1)
                (func (export "grow") (result i32) (memory.grow (i32.const 1))))"#,
        )?;
        let mut store = limited_store(&module, limit)?;
        let instance = Instance::new(&mut store, &module, &[])?;
        let grow = instance.get_typed_func::<(), i32>(&mut store, "grow")?;

        Ok(grow.call(&mut store, ())?)
    }
}
