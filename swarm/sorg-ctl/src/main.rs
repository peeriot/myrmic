use clap::Parser;
use sorg_client::Client;
use sorg_client::utils::zenoh_err;
use sorg_ctl::{Ctl, Result};
use zenoh::{Config, config::WhatAmI};

#[tokio::main]
async fn main() -> Result<()> {
    let ctl = Ctl::parse();
    let client = sorg_client().await?;
    ctl.process_command(client).await?;
    Ok(())
}

async fn sorg_client() -> Result<Client> {
    let mut zenoh_config = Config::default();
    zenoh_config
        .set_mode(Some(WhatAmI::Peer))
        .expect("setting mode cannot fail here");

    let session = zenoh::open(zenoh_config)
        .await
        .map_err(|zen_err| zenoh_err!("opening zenoh session for the CLI", zen_err))?;
    let client = Client::new(session);
    Ok(client)
}
