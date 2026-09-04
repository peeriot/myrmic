use crate::args::Ctx;

#[derive(clap::Parser, Default)]
pub struct List {}

pub async fn handle(ctx: Ctx, _cmd: List) -> anyhow::Result<()> {
    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    let classes = client.list_classes().await?;
    if classes.is_empty() {
        println!("No classes registered");
        return Ok(());
    }

    for class in &classes {
        let hash = class
            .wasm_hash
            .as_ref()
            .map_or("none".to_owned(), |h| h.to_hex());

        println!("{} [{}]", class.name, hash);

        for a in &class.artifacts {
            println!(
                "  platform '{}' (aot: [{}], meta: [{}])",
                a.platform,
                a.aot_hash.to_hex(),
                a.meta_hash.to_hex()
            );
        }
    }

    Ok(())
}
