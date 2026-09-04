# Contributing to Myrmic

Thanks for your interest in contributing to Myrmic! Contributions of all sizes are welcome - bug reports, documentation fixes, demos and examples, and code improvements all matter. This guide explains how to get set up, what we expect in a contribution, and how to get your changes merged.

Myrmic is **experimental and under active development**, so interfaces and internals can change quickly. If in doubt about whether a change fits, open a [Discussion](https://github.com/peeriot/myrmic/discussions) or a draft pull request early and ask.

## Code of Conduct

All participation in this project is governed by our [Code of Conduct](CODE_OF_CONDUCT.md). By taking part, you agree to uphold it. Please report unacceptable behavior to the maintainers at [conduct@myrmic.dev](mailto:conduct@myrmic.dev).

## Ways to Contribute

- **Report bugs** - file a reproducible issue with logs (see below).
- **Improve documentation** - fix gaps, clarify steps, add examples. Docs live under [`doc/chapters/`](doc/chapters/).
- **Share demos and examples** - show real use cases, especially Cell modules under [`tests/fixtures/`](tests/fixtures/).
- **Contribute code** - bug fixes and improvements across the workspace.

If you are new to the GitHub workflow (fork, branch, pull request), see the [Open Source Guide](https://opensource.guide/how-to-contribute/).

## Getting Help

- Ask in [GitHub Discussions](https://github.com/peeriot/myrmic/discussions) or on [Discord](https://discord.gg/zExh79pWgj).
- If you are unsure where to start, ask for a *good first issue*.

## Reporting Bugs

Open a [GitHub Issue](https://github.com/peeriot/myrmic/issues) and include:

- A minimal, reproducible example or a clear sequence of steps.
- What you expected to happen and what actually happened.
- Relevant logs and error output.
- Your environment: target (Linux / ESP32-C6), architecture, and the Myrmic commit or version you are on.

For **security vulnerabilities, do not open a public issue** - follow the [Security policy](SECURITY.md) instead.

## Suggesting Changes and Features

For anything non-trivial, please start a [Discussion](https://github.com/peeriot/myrmic/discussions) before writing code. This lets maintainers give early feedback on direction and avoids wasted effort, which matters while the platform is still evolving.

## Development Setup

Myrmic is a Rust workspace (edition 2024). The pinned toolchain is declared in [rust-toolchain.toml](rust-toolchain.toml) and is installed automatically by `rustup` the first time you build - you do not need to install a Rust version by hand.

Clone and build the OS-targeted workspace:

```bash
git clone https://github.com/peeriot/myrmic.git
cd myrmic
cargo build
```

Additional tooling used by the project's checks:

- [`cargo-nextest`](https://nexte.st/) - test runner (the `citest` alias maps to `nextest run`).
- [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) - license, advisory, ban, and source verification.

```bash
cargo install cargo-nextest cargo-deny
```

Target-specific work needs extra setup:

- **Embedded:** a Rust **nightly** toolchain plus the Espressif toolchain and [`espflash`](https://github.com/esp-rs/espflash). Build and flash via the provided cargo aliases, e.g. `cargo build-c6` / `cargo run-c6`. See the examples under [`embedded/`](embedded/).
- **WebAssembly (Cell modules):** the `wasm32-unknown-unknown` target and a nightly toolchain (the WASM build uses `-Zbuild-std`). See [`sdk/`](sdk/) and the per-component READMEs.

## Before You Submit

Please run the same checks CI runs, so your pull request passes on the first try. These mirror the scripts under [`.ci/`](.ci/):

```bash
# Formatting (must produce no diff)
cargo fmt --all

# Linter - warnings are denied
cargo clippy --all-targets -- --deny warnings

# Documentation - warnings are denied
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items

# Tests
cargo citest          # alias for: cargo nextest run

# Dependency / license verification
cargo deny check
```

For embedded and WASM changes, also run the relevant target checks, e.g.:

```bash
cargo clippy-c6       # or clippy-c5 / clippy-c61
./.ci/check/wasm      # builds and lints the WASM module examples
```

Notes:

- Code must be formatted with `rustfmt` (Unix newlines, edition 2024 - see [rustfmt.toml](rustfmt.toml)).
- Clippy must be **warning-free**; CI runs with `--deny warnings`.
- New dependencies must pass `cargo deny` (acceptable licenses, no banned crates or advisories). See [deny.toml](deny.toml).
- Add or update tests for behavior you change.

## Pull Request Process

1. **Fork** the repository and create a topic branch off `master` (e.g. `fix/cell-mailbox-retry` or `docs/quickstart-typo`).
2. Make focused changes - keep each PR scoped to one logical change where possible. If you need to make multiple changes *related* to each other, try to break them into separate commits with clear messages. If you need to make multiple *unrelated* changes, consider creating separate PRs.
3. Ensure all the checks in [Before You Submit](#before-you-submit) pass locally.
4. Open a pull request against `master` with a clear title and description of **what** changed and **why**. Link any related issues or discussions.
5. Mark the PR as a **draft** while it is work-in-progress. Note that the full CI suite runs once the PR is marked *ready for review* (draft PRs are not fully validated). If you are a **first-time contributor**, GitHub requires a maintainer to manually approve your workflow runs before CI will execute, so there may be a short delay before checks start.
6. A maintainer will review your change, request adjustments if needed, and merge it when it is ready. Code ownership and required reviewers are defined in [`.github/CODEOWNERS`](.github/CODEOWNERS).

If you have questions during review, leave a comment on the PR.

## Review Process

Once your pull request is ready for review, a maintainer will go through it:

1. A maintainer reviews the change and leaves inline comments in the code where something needs attention.
2. The maintainer then either **approves** the pull request or **requests changes**. All CI checks must be green before a pull request can be approved and merged - a maintainer will not approve nor review a PR with failing checks.
3. If changes are requested, read the comments, fix the code accordingly, push your updates, and **re-request review**.
4. When you have addressed a comment, acknowledge it with a short reply or a 👍 reaction so the reviewer knows it is handled.
5. **Do not resolve review comments yourself** - resolving a comment is done by the maintainer who wrote it, once they have confirmed it is addressed.

This loop repeats until the pull request is approved and merged.

## Commit Messages

- Write clear, imperative commit subjects (e.g. "Fix mailbox retry on reconnect").
- Keep the subject concise; use the body to explain context and reasoning when it isn't obvious.
- Reference relevant issues or PRs where helpful (e.g. `(#910)`).

## Documentation Contributions

Documentation is a great entry point. Docs sources live under [`doc/chapters/`](doc/chapters/) and are published as the handbook at [book.myrmic.dev](https://book.myrmic.dev). When writing docs, follow the project style:

- Prefer clarity over completeness, and one clear way over many options.
- Use direct, simple language, active voice, and present tense.
- Address the reader as "you" and avoid hype or marketing language.
- Use canonical terms consistently (see the [Glossary](doc/chapters/12_glossary.md)).
- Use headings and short paragraphs; prefer lists for steps and avoid deeply nested lists.
- Use `TBD` when something is unknown rather than guessing.
- Make sure any code examples are formatted and actually work.

## License of Contributions

The Myrmic platform code is dual-licensed: the open platform under `GPL-2.0-only` together with the `Myrmic Exception`, and Peeriot GmbH additionally licenses Myrmic commercially. To maintain this licensing model, Peeriot GmbH must be able to use and offer contributed code under both open source and commercial license terms.

**Before we can merge your first pull request, you - or, where applicable, your employer - must accept the Contributor License Agreement (CLA).** The CLA bot posts a link to the current text of the agreement on your first pull request; what you read there is exactly what you accept. One CLA covers individuals and organizations:

- **Individuals:** if you contribute on your own behalf, you accept the CLA yourself. The process takes place through GitHub - a CLA bot will prompt you when you submit your first pull request, and you can accept electronically. Nothing to print, sign, or scan. Your agreement is confirmed with each contribution.
- **Organizations:** if you contribute on behalf of your employer, or another organization that owns or controls the relevant rights, the CLA must be accepted electronically by an authorized representative of that organization, which then designates the people who may contribute on its behalf. Someone contributing solely on behalf of such an organization does not need to accept the CLA separately; a separate acceptance is only needed for contributions made in a personal capacity.

In short: The CLA does not transfer ownership of a contribution to Peeriot GmbH. Ownership remains with you or the applicable rightsholder, as the case may be. The CLA grants Peeriot GmbH broad rights to use, modify, distribute, sublicense, and **relicense** the contribution, including under commercial terms or an open-source license. It also requires confirmation that the contributor is authorized to submit the contribution and grant the rights set out in the applicable CLA.

Please note that this information provides only a summary. The applicable CLA contains the legally binding terms and prevails in the event of any inconsistency. Pull requests for which the CLA has not been accepted cannot be merged, including pull requests containing only small fixes and documentation changes.

### AI-assisted contributions

You may use AI-assisted tools when preparing a contribution. You remain responsible for what you submit: review generated code as you would your own, and make sure you hold the rights the CLA asks you to confirm - in particular, do not include code of unknown provenance. For contributions with substantial AI-generated parts, please say so briefly in the pull request description.

---

Thank you for helping make Myrmic better!
