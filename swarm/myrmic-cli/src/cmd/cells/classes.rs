use std::collections::HashMap;

use cell_protocol::{BlobHash, ClassInfo};
use sorg_common::class_registry::{ClassReference, Removal};

use crate::args::Ctx;
use crate::render::class_label;

mod add;
mod delete;
mod info;
mod list;

#[derive(clap::Parser)]
pub struct Classes {
    #[clap(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    List(list::List),
    Add(add::Add),
    #[clap(alias = "remove", alias = "rm")]
    Delete(delete::Delete),
    Info(info::Info),
}

pub async fn handle(ctx: Ctx, cmd: Classes) -> anyhow::Result<()> {
    let cmd = cmd.cmd.unwrap_or(Cmd::List(list::List::default()));

    match cmd {
        Cmd::List(cmd) => list::handle(ctx, cmd).await,
        Cmd::Add(cmd) => add::handle(ctx, cmd).await,
        Cmd::Delete(cmd) => delete::handle(ctx, cmd).await,
        Cmd::Info(cmd) => info::handle(ctx, cmd).await,
    }
}

/// One line describing a class: its kind and what it points to.
fn describe(info: &ClassInfo, names: &HashMap<BlobHash, Vec<String>>) -> String {
    match info {
        ClassInfo::Wasm { aot, spawns } => {
            let mut parts = vec!["wasm".to_owned()];
            for (platform, aot) in aot {
                parts.push(format!("aot {platform}: {}", aot.to_hex()));
            }
            for child in spawns {
                parts.push(format!("spawns {}", class_label(*child, names)));
            }
            parts.join(", ")
        }
        ClassInfo::Aot { platform, crc } => format!("aot {platform} (crc {crc:#010x})"),
    }
}

/// Prints what removing a class did.
pub(super) fn print_removal(
    class: BlobHash,
    removal: &Removal,
    names: &HashMap<BlobHash, Vec<String>>,
) {
    match removal {
        Removal::Removed => println!("Class {} removed", class.to_hex()),
        Removal::Kept(refs) => {
            println!("Class {} kept, still referenced by:", class.to_hex());
            for reference in refs {
                match reference {
                    ClassReference::Name(name) => println!("  name '{name}'"),
                    ClassReference::Instance(sri) => println!("  cell {sri}"),
                    ClassReference::RestartSpec(sri) => println!("  restart spec of {sri}"),
                    ClassReference::Class(parent) => {
                        println!("  class {}", class_label(*parent, names));
                    }
                }
            }
        }
    }
}
