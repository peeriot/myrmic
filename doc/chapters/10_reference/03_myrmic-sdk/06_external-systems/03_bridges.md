# Bridges

> **Availability:** Linux runtime only

A bridge connects a cell to a system outside the swarm: an MQTT broker or an HTTP service. The bridge is declared in a specification file, and the cell reaches it through a client generated from that file.

## Key concepts

A specification file names a broker for MQTT, or a base URL for HTTP, and lists what can be exchanged:

- for MQTT, the ingress topics the cell receives and the egress topics it publishes
- for HTTP, the endpoints with their method, path and body

To understand and learn how to write a bridge specification file, see [Bridge configuration](../../01_configuration/05_bridge-configuration.md).

## Operations

- Generate a client and its types from one or more specification files.
- Bind a client to the bridge it talks to.
- Publish to an egress topic, or call an HTTP endpoint with a callback for its reply.
- Receive an MQTT ingress topic in an event handler.

## Example

```rust
use myrmic_sdk::{Callback, Metadata};

// Read at compile time. Each file's name becomes its client's name.
myrmic_sdk::import!("../specs/http_bridge.yml");
myrmic_sdk::import!("../specs/mqtt_bridge.yml");

#[myrmic_sdk::cmd]
fn fetch(_md: Metadata) -> myrmic_sdk::Result {
    // The name the bridge was deployed under.
    let client = HttpBridgeClient::new("myapp/http-bridge");

    // Returns once the command is sent. The response reaches the callback.
    client.fetch_data("some-payload".into(), Callback::of::<fetched>())?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn fetched(_md: Metadata, reply: FetchDataReply) -> myrmic_sdk::Result {
    myrmic_sdk::info!("{}", reply.result)?;

    Ok(())
}

// An ingress topic arrives as an event, named after its id in the file.
#[myrmic_sdk::evt]
fn receive_request(_md: Metadata, event: ReceiveRequest) -> myrmic_sdk::Result {
    let client = MqttClient::new("myapp/mqtt-bridge");

    // Publishing to an egress topic. Nothing comes back.
    client.publish_response(PublishResponse { data: event.data })?;

    Ok(())
}
```

## Behavior

### Normal

A specification file is read while the cell is compiled.

A call sends a command to the bridge cell, which then talks to the outside system. Success means the command was sent, not that the system answered.

An HTTP reply comes back to the callback the call named. An MQTT publishing has no reply.

### Errors

A specification file that does not parse fails the build.

Publishing to a topic or calling an endpoint fails when the bridge's name is not valid, or when the corresponding command cannot be sent.

### Limits

A client is bound to one bridge instance, named when the client is built. A wrong name fails at the call, not at compile time.

## API documentation

For the macro, see [`import!`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/macro.import.html). For the reply handle an HTTP call takes, see [`Callback`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/struct.Callback.html).
