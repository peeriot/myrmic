# Bridge Configuration
A bridge is a special cell that connects the swarm to the outside world. There are two bridge types:

- **MQTT** - connects to an external MQTT broker.
- **HTTP** - connects to an external HTTP service.

Bridges are defined and configured using a **YAML specification file** that is passed to the [myrmic deploy](../02_myrmic-cli/05_deploy.md) in two ways:
- Standalone - deploy the bridge directly by passing the specification file to the command.
- As part of an application suite - by referencing the bridge definition file inside the application spec file. See [Cell and Application Configuration](./02_cell-and-application-configuration.md#classes).

Both bridge types have different configuration options - covered in the sections below.

## MQTT Bridge
Connects to an external MQTT broker. The cell can send commands to the bridge to publish messages on an MQTT topic, and receive events from the bridge when the broker pushes a message in.

The configuration file for MQTT bridge has the following fields.

- `name` - **Required.** Name of the bridge cell.
- `broker_url` - **Required.** Alias: `broker`. MQTT broker URL.
- `ingress` - **Required.** Alias: `ingresses`. List of inbound subscriptions - each MQTT message received on a subscribed topic is published as a swarm event.
  - `id` - **Required.** The swarm event name published when a message arrives on this topic.
  - `topic` - **Required.** MQTT topic to subscribe to.
  - `qos` *(optional)* - QoS level. Accepts `0`, `1` or `2`. Defaults to `0`.
  - `payload` - **Required.** Maps the MQTT message payload to a field in the event type. Uses the `${type:name}` format - `name` is a field name in the event type, `type` is the encoding (`string`, `json`, or `bytes`). See [Template Syntax](#template-syntax).
- `egress` - **Required.** Alias: `egresses`. List of outbound publishers - each entry defines a command the bridge cell accepts and triggers as an MQTT publish.
  - `id` - **Required.** The command name a cell sends to trigger this publish.
  - `topic` - **Required.** MQTT topic to publish to. Use a static string, or embed `${type:name}` format to insert values from the input into the topic, where `name` is a field name in the command input type, `type` accepts `string`, `json`, `bool`, or a numeric type - or `${db:namespace/database/schema@key}` to read directly from the swarm key-value store. See [Template Syntax](#template-syntax).
  - `qos` *(optional)* - QoS level. Accepts `0`, `1` or `2`. Defaults to `0`.
  - `payload` - **Required.** Maps a field from the command input type to the MQTT message payload. Uses the `${type:name}` format - `name` is a field name in the command input type, `type` is the encoding (`string`, `json`, or `bytes`). See [Template Syntax](#template-syntax).

**Example:**
```yaml
name: my-mqtt-bridge
broker_url: mqtt://localhost:1883
ingress:
  - id: temperature_reading
    topic: sensors/temperature
    qos: 1
    payload: "${string:reading}"
egress:
  - id: set_threshold
    topic: devices/${string:device_id}/threshold
    qos: 1
    payload: "${json:config}"
```

## HTTP Bridge
Connects to an external HTTP service. The cell sends a command to the bridge, which makes an HTTP request and returns the response.

The configuration file has the following fields.

- `name` - **Required.** Name of the bridge cell.
- `base_url` - **Required.** Base URL of the HTTP service.
- `types` *(optional)* - Named types that endpoints can reference by name using [Template Syntax](#template-syntax) - giving your cell typed request and response values.
  - `definitions` - Map of type name to JSON Schema definition. See [JSON Schema](https://json-schema.org/).
- `endpoints` - **Required.** List of HTTP endpoints exposed to the swarm.
  - `id` - **Required.** The command name a cell sends to trigger this endpoint.
  - `request` - **Required.** Outbound request configuration.
    - `method` - **Required.** HTTP method (e.g. `GET`, `POST`).
    - `path` - **Required.** URL path. Use a static string, or embed `${type:name}` format to insert values from the input into the path, where `name` is a field name in the command input type, or a type name from `types.definitions` for `json`. `type` accepts `string`, `json`, `bool`, or a numeric type - or `${db:namespace/database/schema@key}` to read directly from the swarm key-value store. See [Template Syntax](#template-syntax).
    - `query` *(optional)* - Query parameters to include in the request. Add one entry per parameter - the key is the parameter name, the value is a static string or an embedded `${type:name}` format, where `name` is a field name in the command input type, or a type name from `types.definitions` for `json`. `type` accepts `string`, `json`, `bool`, or a numeric type - or `${db:namespace/database/schema@key}` to read directly from the swarm key-value store. Empty by default. See [Template Syntax](#template-syntax).
    - `headers` *(optional)* - Request headers to include in the request. Add one entry per header - the key is the header name, the value is a static string or an embedded `${type:name}` format, where `name` is a field name in the command input type, or a type name from `types.definitions` for `json`. `type` accepts `string`, `json`, `bool`, or a numeric type - or `${db:namespace/database/schema@key}` to read directly from the swarm key-value store. Empty by default. See [Template Syntax](#template-syntax).
    - `body` *(optional)* - Request body. Maps a field from the command input type to the HTTP request body. Uses the `${type:name}` format - `name` is a field name in the command input type, or a type name from `types.definitions` for `json`. `type` is the encoding (`string`, `json`, or `bytes`). No body by default. See [Template Syntax](#template-syntax).
    - `timeout_ms` *(optional)* - Request timeout in milliseconds. No timeout by default.
  - `response` - **Required.** Maps each HTTP status code the endpoint can return to a variant of a generated reply enum. Must be present - use `{}` for an endpoint with no described responses. Each key is a numeric HTTP status code (e.g. `200`, `404`), named as a reply variant by its canonical reason (`200` → `Ok`, `404` → `NotFound`). The reply enum always also gets an `Unknown(u16)` variant, produced for any status the endpoint returns that isn't listed here; it carries the raw status code. Each status's value is either a body template string, or a map:
    - **body shorthand** - A `${type:name}` body template (`type` is `string`, `json`, or `bytes` - for `json`, `name` can also be a type from `types.definitions`); the variant carries just that body. `200: "${json:result}"` is shorthand for `200: { body: "${json:result}" }`.
    - `headers` *(optional)* - Response headers to surface on the variant. Add one entry per header - the key is the header name, the value uses `${string:name}` - `name` names the field on the variant. Empty by default. See [Template Syntax](#template-syntax).
    - `body` *(optional)* - Maps the HTTP response body to the variant's `body` field. Uses the `${type:name}` format - `type` is the encoding (`string`, `json`, or `bytes`), `name` is a field name on the reply variant - for `json`, can also be a type from `types.definitions`. Omit for a status with no body. See [Template Syntax](#template-syntax).

**Example:**

```yaml
name: my-http-bridge
base_url: http://localhost:8080

types:
  definitions:
    Device:
      type: object
      required: [id, name]
      properties:
        id: { type: string }
        name: { type: string }

endpoints:
  - id: store_reading
    request:
      method: POST
      path: /api/devices/${string:device_id}/readings
      headers:
        Authorization: "Bearer ${db:p/secrets/tokens@api_key}"
      body: "${json:reading}"
      timeout_ms: 5000
    response:
      200:
        headers:
          x-request-id: "${string:request_id}"
        body: "${json:result}"
      404: "${json:error}"

  - id: get_device
    request:
      method: GET
      path: /api/devices/${string:device_id}
      headers:
        Authorization: "Bearer ${db:p/secrets/tokens@api_key}"
      timeout_ms: 5000
    response:
      200: "${json:Device}"
      404: "${json:error}"
```

## Template Syntax
From inside your cell, a bridge looks like any other cell - you send commands to it and receive events from it. But behind the scenes, the bridge has to translate those commands and events to and from raw MQTT messages or HTTP requests. Something has to describe that mapping - which field becomes the topic, which becomes the payload, how to decode an incoming message into an event field. Template syntax is how you define that translation, directly in the config.

The format is `${type:name}`:
- `type` - how the value is encoded or decoded (`string`, `json`, `bytes`, or a numeric type). For HTTP bridges with `types` defined, `${json:name}` can reference a type name from `types.definitions`.
- `name` - the field name in the command input, event type, or HTTP reply variant.

Direction determines what `name` refers to:

- **Outbound** (egress `topic` and `payload`, HTTP request `path`, `query`, `headers`, `body`) - `name` is a field name in the command input type.
- **Inbound** (ingress `payload`, HTTP response `headers`, `body`) - `name` is a field name in the event type for MQTT ingress, or on the reply variant for HTTP.

A second format, `${db:namespace/database/schema@key}`, reads a value directly from the swarm's distributed key-value store at runtime. Supported in MQTT egress `topic`, HTTP request `path`, `query`, and `headers`.

## See Also
- [Cell and Application Configuration](./02_cell-and-application-configuration.md) - how to reference a bridge from an application specification
- [Myrmic CLI Reference](../02_myrmic-cli.md) - full reference for all CLI commands
