use textus::Template as _;

use crate::args::Ctx;
use crate::{determine_name, models};

#[derive(clap::Parser)]
pub struct New {
    path: std::path::PathBuf,

    #[clap(long)]
    name: Option<String>,

    #[clap(long)]
    sdk: Option<String>,
}

#[derive(textus::Template)]
#[template(path = "templates/new", strip_suffix = ".tmpl")]
struct TemplateNew<'a> {
    name: &'a str,
    myrmic_sdk: models::CargoDep,
}

pub fn handle(ctx: Ctx, cmd: New) -> anyhow::Result<()> {
    let New { path, name, sdk } = cmd;

    let name = determine_name(name.as_deref(), &path)?;

    validate_name(name)?;

    crate::info!(ctx, "Creating '{}'", name);

    let sdk = crate::utils::resolve_sdk(ctx, sdk.as_deref())?;

    let template = TemplateNew {
        name,
        myrmic_sdk: sdk,
    };

    if let Err(err) = template.render_into(&path) {
        if let Err(io_err) = std::fs::remove_dir_all(&path) {
            return Err(anyhow::Error::new(io_err).context(format!(
                "unable to cleanup after template render failure: {}",
                err
            )));
        }
        return Err(anyhow::Error::new(err).context("failed to render template"));
    }

    Ok(())
}

fn validate_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("<name> is empty");
    }

    let mut chars = name.chars();
    if let Some(ch) = chars.next() {
        if ch.is_ascii_digit() {
            // Technically would be caught via `is_xid_start`, but this is a pretty standard mistake to make...
            anyhow::bail!("<name> cannot start with a digit");
        }
        if !(unicode_ident::is_xid_start(ch) || ch == '_') {
            anyhow::bail!(
                "the first character in <name> must be a Unicode XID start character (most letters or `_`)"
            );
        }
    }
    for ch in chars {
        if !(unicode_ident::is_xid_continue(ch) || ch == '-') {
            anyhow::bail!(
                "<name> must be Unicode XID characters (numbers, `-`, `_`, or most letters)"
            );
        }
    }
    Ok(())
}
