#![no_std]

use myrmic_sdk::{Metadata, Result, Sri, Void};

const RECEIVER_SRI: &str = "trace_example_sink";
const RECEIVER_CMD: &str = "sink";

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    myrmic_sdk::info!("trace-example-intermediate initialized").ok();
    Ok(())
}

#[myrmic_sdk::cmd]
fn intermediate(_md: Metadata) -> Result<()> {
    myrmic_sdk::info!("intermediate command received").ok();

    let sink = Sri::of_path(RECEIVER_SRI).map_err(|_| "invalid sink sri")?;
    myrmic_sdk::send(sink, RECEIVER_CMD, &Void)?;

    myrmic_sdk::info!("sink command sent").ok();

    Ok(())
}
