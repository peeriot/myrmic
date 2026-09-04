# Myrmic Signal Layer IPC — Authoritative Definition

**Interface name:** `myrmic-signal-layer-ipc`
**Version:** 1 (`signal_layer_ipc::PROTOCOL_VERSION`)
**Source of truth:** the items of crate `signal-layer-ipc` (`sdk/signal-layer/signal-layer-ipc`) listed below, at the Official Release this file ships with (see `EXCEPTION-SCOPE.md`).

This interface is the protocol spoken over a Unix domain socket between a generated pipeline program, running as its own process on an operating-system-based device, and the Myrmic platform. **The Official Interface is the wire protocol** — what crosses the process boundary. It consists of exactly the items listed here. Everything else in the crate is Official SDK Library code that implements one side of the protocol; it may be used, modified or replaced without affecting the interface.

## Items that constitute the interface

| Part | Items (crate `signal-layer-ipc`) | What they define |
|---|---|---|
| Protocol constants | `PROTOCOL_VERSION`, `MAX_FRAME_LEN`, `MAX_RESOLVE_NAME_LEN`, `MAX_OUTLET_WRITE_LEN` | The protocol version carried in every request, and the size limits both sides enforce |
| Message types (module `types`) | `Request`, `Response` | The messages exchanged, encoded with `postcard`; `Request` travels from the platform to the pipeline program, `Response` back |
| Frame format (module `framing`) | `write_frame`, `read_frame`, `decode_frame`, `FrameError` | How one encoded message is delimited on the socket: a length-prefixed frame of at most `MAX_FRAME_LEN` bytes |
| Endpoint (module `path`) | `default_socket_path` | Where the socket lives (`/run/peeriot/signal-layer.sock`, with the fallback rule implemented there) |

An Application interacts with Covered Code through this interface when the bytes it exchanges with the platform conform to these items as published — regardless of which code produces them.

## Items that are not part of the interface

`TapClient` and its methods, `TAP_CALL_TIMEOUT`, `RECONNECT_SLA_SECS`, `ClientRead`, `ClientWrite`, `StoreRead`, `StoreWrite`, `serve`, `TapStore`, `OutletStore`, and the modules `client` and `server` as such. They implement the two ends of the protocol and are Official SDK Library code (`MIT OR Apache-2.0`).

## Maintenance

This list is curated: a change to any listed item is a change to the Official Interface and requires a new protocol version and a new Exception Scope. A CI check that verifies the listed items against the crate is a follow-up; until then, reviewers of `sdk/signal-layer/signal-layer-ipc` keep this file in step with the crate.
