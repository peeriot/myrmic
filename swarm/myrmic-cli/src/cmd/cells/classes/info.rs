use anyhow::Context;
use cell_protocol::ClassInfo;

use crate::args::Ctx;

#[derive(clap::Parser)]
pub struct Info {
    name: String,
}

pub async fn handle(ctx: Ctx, cmd: Info) -> anyhow::Result<()> {
    let Info { name } = cmd;

    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    let info = client
        .get_class_info(&name)
        .await
        .with_context(|| format!("unable to fetch info for class '{name}'"))?;

    match info {
        Some(class) => print_class(&class),
        None => println!("Class '{name}' is not registered"),
    }

    Ok(())
}

fn print_class(class: &ClassInfo) {
    println!("Class {}", class.name);

    let wasm = class
        .wasm_hash
        .as_ref()
        .map_or("none".to_owned(), |h| h.to_hex());
    println!("  wasm:      {wasm}");

    if !class.artifacts.is_empty() {
        println!("  artifacts:");
        for a in &class.artifacts {
            println!(
                "    {} (aot: {}, meta: {})",
                a.platform,
                a.aot_hash.to_hex(),
                a.meta_hash.to_hex()
            );
        }
    }
}
