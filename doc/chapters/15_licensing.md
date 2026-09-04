# Licensing

Myrmic is open source, and it is built so that **what you create on it stays yours**. This page explains the licensing model in plain language. It is a guide, not a legal text: the authoritative documents are the license and exception texts in the repository's [`LICENSES/`](https://github.com/peeriot/myrmic/tree/master/LICENSES) directory and the [`EXCEPTION-SCOPE.md`](https://github.com/peeriot/myrmic/blob/master/EXCEPTION-SCOPE.md) file shipped with each release.

## Three rules

1. **Your code stays your code** as long as it works with Myrmic through the official interfaces.
2. **If you change the Myrmic platform itself, share the change** — publish it under the same license, or obtain a commercial license.
3. **Obligations arise only when you distribute.** Using Myrmic internally, testing, evaluating or running it for yourself creates no disclosure duties.

## What is licensed how

| Part of Myrmic | License | What it means for you |
|---|---|---|
| The platform: runtime, self-organization, data layer, embedded ports, firmware, host side of all interfaces | `GPL-2.0-only` together with the **Myrmic Exception** | Copyleft applies to the platform and to changes you make to it. The Exception makes sure it does not reach your applications |
| SDK, interface and contract crates, signal-layer runtime support and bus shims, code generators, standard drivers and steps | `MIT OR Apache-2.0` | Compile them into your own software freely; only the usual permissive notice obligations apply |
| Examples, tutorials, templates | `MIT-0` | Copy, adapt and ship without attribution |
| Documentation | `CC-BY-4.0` | Reuse with attribution. Code examples in the documentation are additionally available under `MIT-0` |

## Why GPLv2 with an exception

A permissive license would let anyone absorb the platform into a closed product. A plain copyleft license would reach into every application built on it. The Myrmic Exception is the middle path: the platform stays open, improvements to it flow back, and the applications, modules and pipelines you build on top are yours to license as you see fit — including proprietary. The Exception also lets Myrmic ship together with Apache-licensed components such as the WebAssembly runtimes, which the GPLv2 alone would not permit.

The Exception does not restrict anything the GPLv2 allows; it only adds permissions. Because it grants additional rights and imposes no new restrictions, the combination continues to meet the Open Source Definition.

## Where to look next

- **[What you can build](15_licensing/01_what-you-can-build.md)** — a decision guide: find your situation and read the verdict.
- **[Contributing and the CLA](15_licensing/02_contributing-and-the-cla.md)** — how contributions work and why we ask for a Contributor License Agreement.
- **[LICENSING.md](https://github.com/peeriot/myrmic/blob/master/LICENSING.md)** in the repository — the plain-language guide the project maintains next to the code.

## What you never have to do

- Publish your own application code because it runs on or with Myrmic.
- Ask us for permission, register, or pay a fee to use Myrmic.
- Add a notice for Peeriot to code that our generators produced for you.
- Accept the Contributor License Agreement just to *use* Myrmic — it is only needed to *contribute*.

## Trademarks

"Myrmic", "EdgeVance" and "Peeriot" are trademarks of Peeriot GmbH. The Myrmic logo and other brand assets are not covered by the software or documentation licenses. A trademark policy describing what you can do with the name and logo will be published separately. Until then, truthful references such as "built on Myrmic" are fine; please ask before using the logo or the name for a product, domain or event.

## License of this documentation

Except where otherwise indicated, the Myrmic documentation is licensed under the Creative Commons Attribution 4.0 International License (CC-BY-4.0). Code examples contained in the documentation — the code blocks on these pages — are additionally licensed under the MIT No Attribution License (MIT-0). You may use each such code example under either CC-BY-4.0 or MIT-0, at your option. These licenses do not apply to third-party material identified as such or to trademarks, logos and other brand assets, which remain subject to their respective terms.

## Questions

Open a [GitHub Discussion](https://github.com/peeriot/myrmic/discussions) or write to [contact@myrmic.dev](mailto:contact@myrmic.dev). Questions about where the line runs are welcome — answering them is cheaper for everyone than a wrong assumption.
