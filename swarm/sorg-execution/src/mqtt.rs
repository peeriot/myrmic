use rumqttc::{AsyncClient, EventLoop, MqttOptions};
use sorg_common::custom_err;

use crate::Result;

pub use sorg_common::MqttConnection;

pub(crate) async fn connect_to_broker(
    mqtt_connection: &MqttConnection,
) -> Result<(AsyncClient, EventLoop)> {
    let MqttConnection {
        broker_address,
        broker_port,
        channel_cap,
        client_id,
        keep_alive_period,
    } = mqtt_connection;
    let mut mqtt_options = MqttOptions::new(client_id, broker_address.as_ref(), *broker_port);
    mqtt_options.set_keep_alive(*keep_alive_period);

    let (client, mut event_loop) = AsyncClient::new(mqtt_options, *channel_cap as usize);
    // poll the event loop to make sure that connection is there
    event_loop.poll().await.map_err(|err| {
        custom_err!(
            "failed to connect to mqtt broker on '{address}':'{port}' - {err}",
            address = broker_address.as_ref(),
            port = broker_port,
        )
    })?;
    Ok((client, event_loop))
}
