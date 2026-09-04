use anyhow::Result;
use common::{Counter, TOPIC_SINK};
use zenoh::Config;

#[tokio::main]
async fn main() -> Result<()> {
    // set up zenoh session
    let config = Config::from_file("./code/subscriber/config.yaml").expect("failed to read config");
    let session = zenoh::open(config)
        .await
        .expect("failed to start zenoh session");

    // subscribe to the sink topic
    let subscriber = session
        .declare_subscriber(TOPIC_SINK)
        .await
        .expect("failed to declare subscriber");

    while let Ok(sample) = subscriber.recv_async().await {
        let counter = Counter::from_payload(&sample.payload().to_bytes())?;
        println!("received the counter {counter:?}");
    }

    unreachable!("Should never error on receive");
}
