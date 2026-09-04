use std::time::Duration;

use anyhow::Result;
use common::{Counter, TOPIC_SOURCE};
use zenoh::Config;

#[tokio::main]
async fn main() -> Result<()> {
    // set up zenoh session
    let config = Config::from_file("./code/publisher/config.yaml").expect("failed to read config");
    let session = zenoh::open(config)
        .await
        .expect("failed to start zenoh session");

    let mut counter = Counter::default();

    // periodically publish the and increment the counter
    loop {
        println!("publishing the counter {counter:?}");
        let payload = counter.to_payload()?;
        session
            .put(TOPIC_SOURCE, payload)
            .await
            .expect("failed to publish counter");

        counter.increment();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
