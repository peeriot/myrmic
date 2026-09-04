# myrmic send

## Name
`myrmic send` - Send a command to a deployed cell

Aliases: `command`, `cmd`

## Synopsis
```
myrmic send [OPTIONS] [SRI/SRN] [NAME] [PAYLOAD]
```

## Description
Send a command to a deployed cell. Commands are fire-and-forget - no response is returned.

- `SRI/SRN` - required. Identifies the target cell, either by its SRI (UUID) or by its SRN name.
- `NAME` - required. Specifies the command to invoke.
- `PAYLOAD` - optional. Specifies data to pass to the command. How the value is encoded is described in [Payload Encoding](#payload-encoding).

A trace ID is printed - use it with [`myrmic telemetry traces`](12_telemetry/02_traces.md) to trace the request.

## Options
`--raw`

Decodes the payload as a hex string (an optional `0x` prefix is allowed) and sends those raw bytes as-is, bypassing JSON encoding. Use this when the command handler expects a non-JSON wire format.

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

Pass `--raw` to send raw bytes instead. The payload is decoded as a hex string (an optional `0x` prefix is allowed) and the resulting bytes are sent as-is, with no JSON encoding.

## Examples
1. Send a command with no payload:

```bash
myrmic send my-cell ping
```

2. Send a JSON object payload:

```bash
myrmic send my-cell my_command '{"count": 10}'
```

3. Send a bare string - it is wrapped as a JSON string automatically:

```bash
myrmic send my-cell greet ada
```

4. Send raw bytes from a hex string, bypassing JSON encoding:

```bash
myrmic send my-cell my_command 0xdeadbeef --raw
```
