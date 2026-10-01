mod pipeline_config {
    #![allow(unused_imports, dead_code, unused_variables)]
    include!(concat!(env!("OUT_DIR"), "/pipeline.rs"));
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    pipeline_config::setup_outlet_registry();
    pipeline_config::setup_tap_registry();
    pipeline_config::spawn_sources(&(), pipeline_config::BoardPeripherals::new());

    println!("Pipeline `site-control-linux` running. Press Ctrl-C to stop.");
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl-c");
}
