# Myrmic Signal Layer Module Contract — Authoritative Definition

**Interface name:** `myrmic-signal-layer-module`
**Version:** identical to the Official Release version this file ships with (see `EXCEPTION-SCOPE.md`)
**Source of truth:** the items of crates `signal-layer-core` (`sdk/signal-layer/signal-layer-core`) and `myrmic-signal-layer-types` (`sdk/signal-layer/signal-layer-types`) listed below, at that Official Release.

This interface is the contract between a signal-layer module — a processing step or a driver — and the platform code it is compiled and linked with into a single firmware image on microcontrollers. **The Official Interface is the contract**: the traits a module implements, the slot and registry types through which values flow, and the wire-type system that gives those values their identity. It consists of exactly the items listed here. Everything else in the two crates is Official SDK Library code and may be used, modified or replaced without affecting the interface.

## Items that constitute the interface

| Part | Items | What they define |
|---|---|---|
| Step contract (`signal-layer-core`) | trait `ProcessingStep` | What a processing step implements and how the platform drives it |
| Slot types (`signal-layer-core`) | `RetainedSlot<T, K>` (`new`, `update`, `clear`, `read`), `BatchSlot<T, N>` (`new`, `push`, `drain`, `dropped`), `EventSlot<T>` (`new`, `emit`, `take`), `Timestamp` | How a module publishes retained values, batches and events |
| Stream-kind markers (`signal-layer-core`) | `StreamKindMarker`, `Signal`, `Metric` | The kinds a retained slot may declare |
| Registry contract (`signal-layer-core`) | `TapRegistry` and `OutletRegistry` (`new`, `register`, `resolve`, `get`, `name_at`, `len`, `is_empty`), `SlotEntry` (`kind`, `wire_type_id`, `retained`, `event`, `batch`), `OutletEntry` (`kind`, `wire_type_id`, `retained`, `write_bytes`), `TapKind`, `TapError`, `MAX_TAPS`, `MAX_OUTLETS` | How module slots are registered by name and resolved by the platform |
| Type-erasure traits (`signal-layer-core`) | `AnyRetained`, `AnyEvent`, `AnyBatch`, `AnyWritable` | How the platform reads and writes slots without knowing their concrete types |
| Wire-type contract (`myrmic-signal-layer-types`) | trait `WireType`, `fnv1a_32`, and the standard payload types `DigitalState`, `PwmDuty`, `OutletFault`, `ThresholdAlarm`, `DriverHealth`, `HealthEvent` | How a value type declares its stable identity and encoding, and the payload types every module may rely on |

An Application interacts with Covered Code through this interface when its modules implement and use these items as published — whether it obtains them from the crates named above or from its own compatible definitions.

## Items that are not part of the interface

The re-exports `signal_layer_core::Arc` (from `portable_atomic_util`) and `signal_layer_core::types` as such, the sealing trait `Sealed`, and any other item of the two crates not listed above. They are Official SDK Library code (`MIT OR Apache-2.0`).

Module descriptors (`descriptor.yaml`), pipeline descriptions and board manifests are data, not part of this interface; see Note 2 of `EXCEPTION-SCOPE.md`.

## Maintenance

This list is curated: a change to any listed item is a change to the Official Interface and belongs to a new Official Release with its own Exception Scope. A CI check that verifies the listed items against the crates is a follow-up; until then, reviewers of `sdk/signal-layer/signal-layer-core` and `sdk/signal-layer/signal-layer-types` keep this file in step with the crates.
