# Running on Linux

On a microcontroller the Signal Layer is part of the firmware, so it is there whenever a cell is.
On Linux it is a separate process, and that difference is the whole of this page.

The pipeline runs as a standalone process that opens the buses, applies the steps, and serves
named values to the runtime over a Unix-domain socket. It has to be running before a cell can
read anything.

## Socket path

Both sides resolve the socket path with the same function, so no configuration is needed when
they run as the same user. The rule, in order:

1. If `/run/peeriot/` exists and is writable by the effective user, use
   `/run/peeriot/signal-layer.sock`.
2. Otherwise, if `$XDG_RUNTIME_DIR` is set, use `$XDG_RUNTIME_DIR/peeriot-signal-layer.sock`.
3. Otherwise fail closed.

There is deliberately no `/tmp` fallback. The socket is protected only by filesystem permissions,
mode `0660`, and a path under `/tmp` would be reachable by any user on the machine.

There is no runtime override. Control the path by setting `XDG_RUNTIME_DIR` or by making
`/run/peeriot/` writable.

The two sides fail differently when no path resolves. The pipeline process aborts, because a
pipeline nobody can reach is not useful. The runtime keeps running and reports taps as
unavailable.

## Connection behaviour

The runtime connects lazily, on a cell's first tap call. If the pipeline is not running,
resolving a tap returns "not found" and reads return unavailable, which are the same answers a
cell already handles for a name that does not exist. **A cell therefore cannot distinguish a
missing tap from a missing pipeline.**

Reconnection is also lazy. The host retries on the next tap call after the socket becomes
reachable, so a cell that has stopped calling will not reconnect on its own.

Every call is bounded at five seconds. A cell polling a stalled pipeline will stall with it for
that long per call.

Handles do not survive a reconnect. After the connection is rebuilt, a read through a handle
issued before it returns an error, and the cell should resolve the name again.

## One caution on startup

The socket is created before the buses are opened, and a failure to open a bus is fatal. So a
pipeline that is about to abort can briefly present a listening socket. A cell connecting in that
window will succeed and then find the pipeline gone.

## What Linux supports

| | Embedded | Linux |
|---|---|---|
| Sensors over I²C | yes | yes |
| Sensors over SPI | yes | yes |
| Actuators (outlets) | yes | yes |
| Processing steps | yes | yes |
| Batch taps | reserved | reserved |

The board file differs too: on Linux a bus names a kernel device path instead of pins -
`/dev/i2c-1` for I²C, `/dev/spidev0.0` for SPI - and `chip:` is `linux`. An SPI device still
declares its chip-select as a GPIO under `pins.cs`, exactly as on embedded: the kernel's own
chip-select is disabled and the Signal Layer drives the line itself around each transfer. The device list carries over unchanged, because a sensor
keeps the same driver and the same address wherever it is plugged in.
