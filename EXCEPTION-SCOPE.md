# Myrmic Exception — Exception Scope

**Exception Version:** Myrmic Exception 1.0
**Official Release:** Myrmic 0.4.0

The release version above is the only version number stated in this file. Below, "the Release Version" means that version: every interface, Official SDK Library and Designated Generator listed here carries it, unless a different version is stated explicitly.

This file is the Exception Scope published as part of the Official Release identified above. It applies to that Official Release and to any modified version based on that Official Release to which the Myrmic Exception has been extended.

This file identifies the **Official Interfaces**, **Official SDK Libraries**, **Approved Licenses** and **Designated Generators** applicable to that release. It does not amend the Myrmic Exception or independently grant, restrict or modify any rights or obligations. Peeriot GmbH may publish an expanded version of this file for the same Official Release that adds entries; it may not remove or restrict entries for this release (Myrmic Exception, Section 1.6).

Every Official SDK Library named below is a crate of the Myrmic workspace at the Release Version. Where a crate is published on crates.io, the published version is identical to the workspace version.

## 1. Official Interfaces and Official SDK Libraries

### Interface 1 — WASM Host Interface

The host functions a WebAssembly Cell imports from the Myrmic runtime, used by Applications executed in a sandbox: dynamically loaded on operating-system-based devices, or ahead-of-time compiled and embedded in a firmware image on microcontrollers.

| Property | Value |
|---|---|
| Name | `myrmic-wasm-host` |
| Version | the Release Version |
| Authoritative interface definition | [`legal/interfaces/wasm-host-interface.md`](legal/interfaces/wasm-host-interface.md) — import namespaces `arguments`, `ble`, `cell`, `db`, `error`, `gateway`, `gpio`, `logging`, `outlet`, `tap`, `time` (61 functions) |

Official SDK Libraries for this interface:

| Library | License |
|---|---|
| `myrmic-sdk` | MIT OR Apache-2.0 |
| `myrmic-sdk-macros` | MIT OR Apache-2.0 |
| `myrmic-common` | MIT OR Apache-2.0 |

### Interface 2 — Signal Layer IPC

The protocol between a generated pipeline program, running as its own process on an operating-system-based device, and the Myrmic platform.

| Property | Value |
|---|---|
| Name | `myrmic-signal-layer-ipc` |
| Version | 1 (`signal_layer_ipc::PROTOCOL_VERSION`) |
| Authoritative interface definition | [`legal/interfaces/signal-layer-ipc.md`](legal/interfaces/signal-layer-ipc.md) — the wire protocol of crate `signal-layer-ipc` at the Release Version: protocol constants, message types, frame format and endpoint |

Official SDK Libraries for this interface:

| Library | License |
|---|---|
| `signal-layer-ipc` | MIT OR Apache-2.0 |
| `signal-layer-linux-rt` | MIT OR Apache-2.0 |
| `linux-i2c-shim`, `linux-spi-shim`, `linux-gpio-shim` | MIT OR Apache-2.0 |
| `signal-layer-core` | MIT OR Apache-2.0 |
| `myrmic-signal-layer-types` | MIT OR Apache-2.0 |

### Interface 3 — Signal Layer Module Contract

The contract implemented by processing steps and drivers that are compiled together with platform code into a single firmware image on microcontrollers (the `ProcessingStep` trait, the tap and outlet registries, slot types and the wire-type contract).

| Property | Value |
|---|---|
| Name | `myrmic-signal-layer-module` |
| Version | the Release Version |
| Authoritative interface definition | [`legal/interfaces/signal-layer-module-contract.md`](legal/interfaces/signal-layer-module-contract.md) — the step contract, slot and registry types and wire-type contract of crates `signal-layer-core` and `myrmic-signal-layer-types` at the Release Version |

Official SDK Libraries for this interface:

| Library | License |
|---|---|
| `signal-layer-core` | MIT OR Apache-2.0 |
| `myrmic-signal-layer-types` | MIT OR Apache-2.0 |

## 2. Approved Licenses

| License | SPDX identifier |
|---|---|
| BSD Zero Clause License | `0BSD` |
| Apache License 2.0 | `Apache-2.0` |
| Apache License 2.0 with LLVM exception | `Apache-2.0 WITH LLVM-exception` |
| BSD 2-Clause "Simplified" License | `BSD-2-Clause` |
| BSD 3-Clause "New" or "Revised" License | `BSD-3-Clause` |
| Boost Software License 1.0 | `BSL-1.0` |
| Creative Commons Zero v1.0 Universal | `CC0-1.0` |
| ISC License | `ISC` |
| MIT License | `MIT` |
| Unicode License v3 | `Unicode-3.0` |
| The Unlicense | `Unlicense` |
| zlib License | `Zlib` |
| Mozilla Public License 2.0 | `MPL-2.0` |
| Eclipse Public License 2.0 | `EPL-2.0` |
| OpenSSL License, including the Original SSLeay License it incorporates | `OpenSSL` |
| Blue Oak Model License 1.0.0 | `BlueOak-1.0.0` |

Data licenses are not Approved Licenses: a data package distributed alongside Covered Code is not a work based on the Covered Code, so no additional permission is needed for it.

## 3. Designated Generators

| Generator | Produces |
|---|---|
| `myrmic-sdk-macros` | Compile-time expansion inserted into Cell code (procedural macros) |
| `pipeline-codegen` | Pipeline wiring code, target-independent |
| `esp-codegen` | Pipeline backend for ESP32 firmware targets |
| `linux-codegen` | Stand-alone pipeline project for operating-system-based devices |

## 4. Notes

1. The standard modules under `signal-modules/` (drivers and processing steps) are licensed `MIT OR Apache-2.0` and are not exception-relevant.
2. Pipeline descriptions, module descriptors and board manifests are data, not program code; no permission is needed for them.
3. The additional permission depends on the Application interacting with the Covered Code exclusively through the unmodified Official Interfaces listed above (Myrmic Exception, Section 2.1). Whether an Official SDK Library is used, modified or replaced does not by itself decide this. Interfaces not listed here, and modified versions of the interfaces listed here, are not Official Interfaces.
4. Each Official Release carries its own `EXCEPTION-SCOPE.md`; the file shipped with a release governs that release. Anyone exercising a permission under Section 2 or Section 3 of the Myrmic Exception must provide recipients with an unmodified copy of this file together with the Covered Code.
