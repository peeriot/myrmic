# Myrmic Signal Layer Types

Shared Signal Layer wire types, serialized with [`postcard`](https://crates.io/crates/postcard)
across the host/WASM boundary: digital and PWM outlet commands, outlet faults,
driver health and threshold-alarm events. Written by an embedded host into tap
and outlet slots, and decoded by WASM cells without allocation.

Published as `myrmic-signal-layer-types`; the library is still imported as
`signal_layer_types`.
