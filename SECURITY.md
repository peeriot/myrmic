# Security Policy

We take the security of Myrmic seriously. This document explains how to report a vulnerability, what to expect after you report it, and the boundaries of what we consider a security issue.

Myrmic is developed following a secure product development lifecycle aligned with [IEC 62443-4-1](https://webstore.iec.ch/en/publication/33615). See the [Security documentation](doc/chapters/11_security.md) for the security mechanisms the preview implements.

## Project Status

Myrmic is **experimental and under active development** and has not yet had a stable release. Security fixes are applied to the latest `master` and the most recent pre-release only; older snapshots are not maintained. Until a stable release exists, please verify issues against the latest `master` before reporting.

## Reporting a Vulnerability

**Please report security vulnerabilities privately. Do not open public issues, discussions, or pull requests for them**, as that can expose other users before a fix is available.

Use either of these private channels:

- **GitHub Security Advisories (preferred):** open a private report via the repository's [Security Advisories](https://github.com/peeriot/myrmic/security/advisories/new) page. This keeps the report confidential and lets us collaborate on a fix in one place.
- **Email:** [security@myrmic.dev](mailto:security@myrmic.dev). If you would like to encrypt your report, ask for our PGP key in an initial message.

Please include as much of the following as you can:

- A description of the vulnerability and its potential impact.
- The affected component, the target you ran on (see [Supported Targets](doc/chapters/01_quickstart.md#supported-targets)), and the commit or version you tested.
- Step-by-step instructions to reproduce, ideally with a minimal proof of concept.
- Any logs, crash output, or configuration needed to trigger the issue.
- Whether the issue is already publicly known and any disclosure deadline you intend to follow.

## What to Expect

- **Acknowledgement:** we try to acknowledge reports promptly, typically within a few business days. Myrmic is a community project and comes without response-time or support commitments; we handle reports on a best-effort basis.
- **Assessment:** we will investigate, confirm the issue, and keep you informed of our findings and the expected timeline.
- **Resolution:** we will work on a fix and coordinate a release. Timelines depend on severity and complexity; we will communicate if a fix will take longer.
- **Confidentiality:** we will keep your report private and handle it on a need-to-know basis.

## Coordinated Disclosure

We follow a coordinated disclosure model. Please give us a reasonable opportunity to investigate and release a fix before disclosing the issue publicly. We will agree on a disclosure timeline with you and, unless you prefer to remain anonymous, credit you for the discovery once the issue is resolved.

## Scope

In scope are vulnerabilities in this repository's code and the components it builds - the runtime, self-organization, data, peer-to-peer, and transport layers, the CLIs, and the WebAssembly SDK and tooling.

Please keep the following in mind:

- **Documented limitations are not vulnerabilities.** Myrmic targets soft real-time, non-critical processes, eventual consistency, and resilience only up to high availability. It provides no functional-safety guarantees and no certified safe-state mechanism. These are deliberate design boundaries (see [Scope & Limitations](README.md#scope--limitations) and [Guarantees](doc/chapters/08_guarantees.md)), not security defects.
- **Third-party dependencies.** Vulnerabilities in upstream dependencies (e.g. Wasmtime, WAMR, Zenoh) are best reported to those projects directly. If a dependency issue affects Myrmic specifically, let us know so we can track and mitigate it.

## Safe Harbor

We consider security research conducted in good faith - accessing only your own data, avoiding privacy violations and service disruption, and giving us a reasonable time to respond before public disclosure - to be authorized. We will not pursue or support legal action against researchers who follow this policy.
