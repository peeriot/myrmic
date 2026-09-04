# myrmic publish

## Name
`myrmic publish` - Publish an event

Aliases: `event`, `pub`

## Synopsis
```
myrmic publish [OPTIONS] [NAME] [PAYLOAD]
```

## Description
Publish an event. Unlike [`myrmic send`](08_send.md), publish is a broadcast - it is not targeted at a specific cell and does not wait for a response.

- `NAME` - required. Specifies the event name.
- `PAYLOAD` - optional. Specifies data to publish with the event. How the value is encoded is described in [Payload Encoding](#payload-encoding).

## Options
`--raw`

Decodes the payload as a hex string (an optional `0x` prefix is allowed) and publishes those raw bytes as-is, bypassing JSON encoding. Use this when the event handler expects a non-JSON wire format.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Payload Encoding

By default, the payload is sent as JSON. A value that is not valid JSON is automatically wrapped as a JSON string:

| Input           | Sent as                        |
| --------------- | ------------------------------ |
| `{"count": 1}`  | JSON object `{"count":1}`      |
| `[1, 2, 3]`     | JSON array `[1,2,3]`           |
| `42`            | JSON number `42`               |
| `true`          | JSON boolean `true`            |
| `hello`         | JSON string `"hello"`          |

Pass `--raw` to publish raw bytes instead. The payload is decoded as a hex string (an optional `0x` prefix is allowed) and the resulting bytes are sent as-is, with no JSON encoding.

## Examples
1. Publish an event with no payload:

```bash
myrmic publish my_event
```

2. Publish an event with a JSON object payload:

```bash
myrmic publish my_event '{"count": 10}'
```

3. Publish an event with raw bytes from a hex string:

```bash
myrmic publish my_event 0xdeadbeef --raw
```
