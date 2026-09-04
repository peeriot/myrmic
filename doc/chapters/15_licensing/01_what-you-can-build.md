# What You Can Build

This guide walks you through the questions that decide what the Myrmic licenses ask of you. Find your situation, follow the letters, and read the verdict. It is a plain-language guide, not legal advice; the authoritative texts are the GNU GPL v2, the [Myrmic Exception](https://github.com/peeriot/myrmic/blob/master/LICENSES/LicenseRef-Myrmic-exception-1.0.txt) and the [`EXCEPTION-SCOPE.md`](https://github.com/peeriot/myrmic/blob/master/EXCEPTION-SCOPE.md) of the release you use.

## The short version

| You want to… | Verdict |
|---|---|
| Use Myrmic internally, evaluate it, run it for yourself | Nothing to do. Obligations arise only when you distribute |
| Ship your own Cell (WASM), pipeline or module built on the official interfaces — on a gateway, in a firmware image, as a product | **Your code stays yours.** License it as you like, including proprietary |
| Ship Myrmic itself (unmodified) together with your product | Pass on the GPL obligations for the platform part: source, notices, and the unmodified `EXCEPTION-SCOPE.md` |
| Change the platform and ship the result | Publish your changes under GPL-2.0-only, or obtain a commercial license |
| Use the output of the Myrmic code generators | It is yours, without conditions |

## A — Are you distributing?

**Distributing** means giving software to someone outside your organization: selling a device with Myrmic on it, offering a download, handing a firmware image to a customer, publishing a container image.

- **No** — you run Myrmic yourself, in your own devices, your own network, your own cloud, or you are evaluating it. → **No obligations.** The GPL asks nothing of you until you distribute. This includes offering a service that runs on Myrmic without giving the software itself to your users.
- **Yes** → continue with **B**.

## B — What are you distributing?

- **B.1 — Unmodified Myrmic**, alone or alongside your product → go to **E**.
- **B.2 — A modified Myrmic platform** (you changed code in the GPL zone: runtime, self-organization, data layer, embedded ports, firmware, host side of the interfaces) → **the GPL applies to your changes.** When you distribute, make the source of your modifications available under GPL-2.0-only, or obtain a commercial license. You may extend the Myrmic Exception to your changes or not — that is your choice (Exception, Section 5); if you do not, remove or qualify notices that would suggest it applies. Then also go to **E** for the notices.
- **B.3 — Your own application** — a Cell, a signal-layer module, a pipeline, a driver — that works *with* Myrmic → continue with **C**.

## C — Does your application talk to Myrmic only through the official interfaces?

The official interfaces are the WASM host interface, the signal-layer IPC protocol and the signal-layer module contract, each defined in the `EXCEPTION-SCOPE.md` of your release. **What must stay unmodified is the interface itself** — the host functions, the protocol, the contract as defined there. The SDK crates that implement them (`myrmic-sdk`, `signal-layer-core`, `signal-layer-ipc`, …) are libraries: use them as published, patch them, or replace them with your own bindings, as long as what crosses the boundary is the unmodified official interface.

- **Yes** — your code interacts with the platform only through those interfaces and does not copy, modify or incorporate platform code → continue with **D**.
- **No, I changed an interface itself or the runtime** to add functionality my application needs → the interface change is a platform modification; treat it under **B.2**. Your application itself is only covered by the Exception if it uses the unmodified official interfaces. If you have forked Myrmic and extended its interfaces, Peeriot's additional permissions do not automatically cover applications written against your extended interfaces. You can grant your own permissions for your changes — but you cannot enlarge Peeriot's Exception (Section 5.3). *In doubt: propose the interface change upstream so it becomes official in the next release.*
- **No, my code reaches into internal APIs or copies platform code** → it is a derivative of the platform; see **B.2**.

## D — How is your application combined with Myrmic?

All four forms are expressly permitted by the Myrmic Exception. Your application may be licensed under terms of your choice, including proprietary.

- **D.1 — A WASM Cell**, compiled with the SDK, loaded dynamically on an operating-system-based device or AOT-compiled and embedded in a firmware image. → **Yours.** The SDK crates inside your module stay MIT/Apache-2.0 and carry the usual notice obligations.
- **D.2 — A firmware image** in which your modules (drivers, steps) are statically linked with the platform. → **Yours.** Building and distributing the combined image is what the Exception is for. For the platform part inside the image, go to **E**.
- **D.3 — A generated pipeline program** running as its own process on a Linux device and talking to the platform over IPC. → **Yours.** The generated program consists of your code plus permissive crates; the generator output is yours without conditions.
- **D.4 — A plugin loaded into a platform process**, communicating only through an official interface. → **Yours.**

Then continue with **E** if you ship the platform along with your application; otherwise you are done.

## E — You ship the platform: what to pass on

When you distribute Myrmic platform code — unmodified or modified, alone or inside a firmware image or container — the GPL-2.0-only obligations apply **to the platform part**:

1. **Source code for the platform part** — either included, or a written offer, or (for unmodified releases) a pointer to the exact upstream release. Your own application's source is *not* affected when **C** and **D** apply.
2. **The license texts** — GPL-2.0-only and the Myrmic Exception, as shipped in the release's `LICENSES/` directory.
3. **The unmodified `EXCEPTION-SCOPE.md`** of the release you rely on. Passing it on unchanged is the one condition the Exception attaches to using its permissions.
4. **Existing copyright and license notices** stay in place, including the third-party notices in `NOTICE`.

The exception does not require you to publish anything about *your* application, and it does not require you to add attribution to Peeriot in your product beyond keeping the notices.

## F — Third-party components in the same image

Combining Myrmic with third-party software is covered in two ways:

- Components under an **Approved License** (Apache-2.0, MIT, BSD, MPL-2.0, EPL-2.0 and the others listed in `EXCEPTION-SCOPE.md`) may be combined and distributed with the platform; each keeps its own license.
- **Operating-system and hardware-access components** — kernels, libc, drivers, bus and radio libraries used through their documented interface — are Support Components and are covered likewise.

For anything else, especially components under licenses not on the list, check compatibility with GPL-2.0-only or ask us; we can add licenses to the list for a release, and additions apply retroactively to that release.

## Worked examples

**A start-up ships an ESP32 sensor node with a proprietary filtering step.** The step implements the unmodified signal-layer module contract (the traits and types listed in the release's `EXCEPTION-SCOPE.md`) and is linked into one firmware image with the platform. → **C** yes, **D.2**: the step stays proprietary. **E**: the firmware ships with the platform source (or a pointer to the Myrmic release), the license texts and the release's `EXCEPTION-SCOPE.md`.

**An integrator installs Myrmic on an industrial gateway and adds a closed Cell for a customer.** → **D.1**: the Cell is theirs. The unmodified Myrmic on the gateway falls under **B.1/E**; pointing to the upstream release and keeping the notices satisfies it.

**A company changes the runtime's scheduling to fit its workload and sells devices with the result.** → **B.2**: the scheduling change is a platform modification. Publish the change under GPL-2.0-only (ideally as an upstream pull request) or obtain a commercial license.

**A hobbyist forks Myrmic, adds a new host function and writes Cells against it.** → **C** no: the new host function is a platform modification (**B.2**, GPL applies if distributed), and the fork's extended interface is not an official interface. The fork author may extend the Exception to their own changes but cannot widen Peeriot's Exception to the new interface. The clean route: propose the host function upstream.

**A team generates a Linux pipeline project with `linux-codegen` and sells it as part of a closed product.** → Generator output is theirs (Exception, Section 4), the generated program is **D.3**; only the MIT/Apache-2.0 notices of the crates it uses need to travel with it.

## Still unsure?

Describe your setup in a [GitHub Discussion](https://github.com/peeriot/myrmic/discussions) or write to [contact@myrmic.dev](mailto:contact@myrmic.dev). Where a real gap shows up, we would rather widen the `EXCEPTION-SCOPE.md` than have you guess.
