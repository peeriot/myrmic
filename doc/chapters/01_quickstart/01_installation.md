# Installation

This page installs the **Myrmic CLI** on Linux. The CLI is the only thing you install by hand:

- The **Myrmic Runtime** is managed by the CLI - `myrmic runtimes start` brings one up, there is nothing separate to install.
- The **Myrmic SDK** is a Rust dependency of your cell crate - `myrmic new` puts it into the generated `Cargo.toml`.

On x86_64 and arm64 (64-bit, including a Raspberry Pi on a 64-bit OS) the release package is the shortest path. On any other architecture, [build from source](#install-from-source).

## Prerequisites

On a fresh Debian or Ubuntu image, run `sudo apt update` once before the first `apt install` on this page. The commands on this page use `curl`. Most images ship it already - check with `curl -V`. If yours does not: `sudo apt install curl` on Debian and Ubuntu, `sudo dnf install curl` on RHEL, AlmaLinux and Fedora.

### To build cells

You need all of these whether you installed the CLI from a package or built it yourself.

- **Rust**, via [rustup](https://rustup.rs/):

  ```sh
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

  The installer does not touch the shell it runs in, so make `rustup` available in the current one before you go on (a new shell picks it up by itself):

  ```sh
  source "$HOME/.cargo/env"
  ```

- **The pinned stable toolchain and the `wasm32v1-none` target** are installed for you on the first build. A cell scaffolded by `myrmic new`, on its first `myrmic build`, ships a `rust-toolchain.toml` that prompts `rustup` to auto-install the right toolchain and target.

  The pinned version is not a technical floor; it is the release tested for the current Myrmic release, and it moves forward with each release.

  To install these ahead of that first build - on an offline machine, or just to keep the build output quiet - the steps are optional:

  ```sh
  rustup toolchain install 1.98.1
  rustup target add wasm32v1-none --toolchain 1.98.1
  ```

- **A C toolchain.** A cell compiles to WebAssembly, but cargo still compiles and links every dependency's build script as a native binary for your machine. rustup ships no linker, so without one `myrmic build` stops before it starts the build, with `no C linker found`.

  | Distribution family | Install |
  |---|---|
  | Debian, Ubuntu | `sudo apt install build-essential` |
  | RHEL, AlmaLinux, Fedora | `sudo dnf install gcc` |

### Additionally, to build the CLI from source

| Distribution family | Install |
|---|---|
| Debian, Ubuntu | `sudo apt install cmake git` |
| RHEL, AlmaLinux, Fedora | `sudo dnf install cmake git` |

`git` clones the repository, and the build shells out to it for the source revision it stamps into the binary. `cmake` is used by a C dependency of the CLI binary.

Both package lists above are derived from Myrmic's dependency tree and from installs observed on a handful of images. They are not a minimal set - if your machine already has a working C compiler, `cmake` and `git`, however they got there, that works too.

### Build resources

These are lab measurements on small cloud instances, not guaranteed minimums - they move with your CPU, disk and how busy the machine is.

- **A cell build takes seconds.** Once the C toolchain is present, `myrmic build` was 9 to 10 seconds on 2 to 4 vCPU, and up to around 30 seconds on a busier host. The first build in a fresh crate also compiles the cell's dependencies, so it is the slow one; later builds are quicker.
- **Building the CLI from source is the heavy step**, and wants a few GB of both RAM and disk. On 8 vCPU running alone it took about three and a half minutes, roughly 2 GB peak resident memory and 2.6 GB left in `target/`. On a contended host it takes proportionally longer - around 9 to 10 minutes when three builds shared a 16-thread machine.

## Install from a release package

Packages are published on the [releases page](https://github.com/peeriot/myrmic/releases) under the tag `myrmic/v<version>`. Each release carries:

| Asset | What it is |
|---|---|
| `myrmic-cli_<version>_<arch>.deb` | the CLI; `<arch>` is `amd64` (x86_64) or `arm64` (aarch64) |
| `myrmic-cli-dbgsym_<version>_<arch>.deb` | its debug symbols |
| four `.rpm` files, two per architecture | the same CLI and its debug symbols, converted from the `.deb`; the converter picks their names, so read them off `SHA256SUMS` |
| `myrmic-cli_<version>_sbom.spdx.json`, `myrmic-cli_<version>_sbom.cdx.json` | the dependency inventory of the build |
| `SHA256SUMS` | checksums for every asset above |

The package name is `myrmic-cli` and the binary it installs is `/usr/bin/myrmic`. It also puts `NOTICE` and the licence texts under `/usr/share/doc/myrmic-cli/`, where the packaging tool adds a copyright file of its own. There is no systemd unit, no man page and no shell completion - `dpkg -L myrmic-cli` lists the full payload.

### Download

The assets hang off the release tag, so the tag is part of the URL. Start with `SHA256SUMS`: it lists every asset of the release by name, which is also how you learn what the RPMs are called - the converter picks those names, nothing in the pipeline records them. Replace `<version>` with the release you want:

```bash
version='<version>'
base=https://github.com/peeriot/myrmic/releases/download/myrmic/v$version

curl -fLO "$base/SHA256SUMS"
cat SHA256SUMS
```

`-f` matters: without it curl saves GitHub's 404 page under the asset's name and still exits 0, so a typo in `$version` gets you a `SHA256SUMS` that reads `Not Found`. With it, curl prints the error and exits non-zero.

Then fetch the package you want from the same place:

Debian, Ubuntu:

```bash
arch="$(dpkg --print-architecture)"
curl -fLO "$base/myrmic-cli_${version}_${arch}.deb"
```

RHEL, AlmaLinux, Fedora:

```bash
curl -fLO "$base/<the .rpm name SHA256SUMS listed>"
```

### Verify the download

With the package and `SHA256SUMS` in the same directory:

```bash
sha256sum --ignore-missing -c SHA256SUMS
```

`SHA256SUMS` lists every asset of the release. Without `--ignore-missing`, `sha256sum` prints `FAILED open or read` for each asset you did not download, and exits non-zero.

### Install

Debian, Ubuntu:

```bash
sudo apt install "./myrmic-cli_${version}_${arch}.deb"
```

RHEL, AlmaLinux, Fedora:

```bash
sudo dnf install "./<the .rpm name SHA256SUMS listed>"
```

The `.deb` line reuses the `$version` and `$arch` you set above. For the RPM, pass the name you read off `SHA256SUMS`. If you would rather not type it out, a glob covers the release suffix the converter picked:

```bash
sudo dnf install "./myrmic-cli-${version}"-*."$(uname -m)".rpm
```

### Distributions

The `.deb` declares `libc6 (>= 2.17)` on both architectures, and the release pipeline checks the binary against that floor before anything is published, so the floor cannot drift. glibc 2.17 is the oldest glibc Rust itself supports, so every distribution the commands on this page work on meets it - Debian 9 or newer, Ubuntu 16.04 or newer, RHEL 8 or newer, and every 64-bit Raspberry Pi OS. The package you downloaded states it too:

```bash
dpkg-deb -f "myrmic-cli_${version}_${arch}.deb" Depends
```

Installing the package has been observed to work on Ubuntu 22.04 and 24.04, Debian 12, and AlmaLinux 9 and 10. Other distributions that meet the floor should work as well, but have not been checked.

Below the floor, `apt` refuses the `.deb` with a `libc6` dependency error naming the version your system has. The RPMs carry the same floor as per-symbol requirements such as `libc.so.6(GLIBC_2.17)(64bit)`, so `dnf` refuses them below it as well; `rpm -qp --requires <file>.rpm` shows them. Below the floor, build from source.

### 32-bit ARM

No package is published for 32-bit ARM (armhf, for example a 32-bit Raspberry Pi OS). There, build from source.

## Install from source

Install the C toolchain and the two extra packages from [Prerequisites](#prerequisites) first, then:

```bash
git clone https://github.com/peeriot/myrmic.git
cd myrmic
cargo build --release --bin myrmic
```

The binary is at `target/release/myrmic`. Put it on your `PATH`, or install it directly:

```bash
cargo install --path swarm/myrmic-cli/
```

To export logs, traces and metrics to tools such as Grafana or Jaeger over OTLP, add `--features open-telemetry` to either command. See the [Observability tutorial](../04_tutorials/07_observability.md).

### With BLE support

To run cells that talk to Bluetooth Low Energy peripherals, the CLI has to be built with its `ble` feature. The release packages are built without it, so BLE support on Linux requires building from source. It links against the system D-Bus library to reach the BlueZ stack, so install its development headers first:

| Distribution family | Install |
|---|---|
| Debian, Ubuntu | `sudo apt install libdbus-1-dev` |
| RHEL, AlmaLinux, Fedora | `sudo dnf install dbus-devel` |

Then install the CLI with the feature:

```bash
cargo install --path swarm/myrmic-cli/ --features ble
```

Without the feature, BLE support is compiled out of the runtime, so it does not advertise `ble` and cannot serve BLE cells. See [Work with BLE peripherals](../05_guides/12_ble.md) for how a cell uses BLE.

## Verify the installation

```bash
myrmic --version
```

## What Myrmic writes to your machine

| Location | Contents |
|---|---|
| `~/.local/share/myrmic/` | runtime identities in `runtimes/<name>.yaml`, and one directory per runtime holding its database (`<id>/db`) and its rotating logs (`<id>/logs/runtime.<date>.log`, rotated daily, 7 kept) |
| `$XDG_RUNTIME_DIR/myrmic/` | the PID file of each running runtime |

Those are the persistent locations at their defaults. A runtime can be given a database directory and a log directory of its own, and `myrmic runtimes start --tmp` keeps the database in memory instead. `$XDG_DATA_HOME` overrides the data directory when it is set. When `$XDG_RUNTIME_DIR` is not set - which is common for a process started by systemd - PID files go to `$TMPDIR/myrmic-<user>/`, which is `/tmp/myrmic-<user>/` unless `$TMPDIR` says otherwise. `<user>` is `$USER`, or `$LOGNAME` if that is unset - and if neither is set, the directory is just `myrmic`.

Builds and deploys also unpack scratch directories under `$TMPDIR` (`myrmic-nest-<uuid>/`, `myrmic-spawn-<uuid>/`), and a runtime using the signal layer opens a socket at `/run/peeriot/signal-layer.sock` or `$XDG_RUNTIME_DIR/peeriot-signal-layer.sock`.

## Uninstall

If a runtime is running, stop it first - with nothing running the command exits with an error, which is harmless here:

```bash
myrmic runtimes stop
```

Remove the CLI:

```bash
sudo apt remove myrmic-cli    # Debian, Ubuntu
sudo dnf remove myrmic-cli    # RHEL, AlmaLinux, Fedora
```

If you installed it with `cargo install`:

```bash
cargo uninstall myrmic-cli
```

Neither removes what Myrmic wrote to your machine. To remove that as well:

```bash
[ -n "${XDG_DATA_HOME:-$HOME}" ] && rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/myrmic"
[ -n "$XDG_RUNTIME_DIR" ] && rm -rf "$XDG_RUNTIME_DIR/myrmic"
rm -rf "${TMPDIR:-/tmp}/myrmic-${USER:-$LOGNAME}" "${TMPDIR:-/tmp}/myrmic"
```

The first line deletes the runtime database - everything your cells stored is in there. The last two cover both PID locations. The guards on the first two lines matter, because an unguarded version of either deletes a root-anchored path when its variables are unset: `rm -rf "$XDG_RUNTIME_DIR/myrmic"` reads as `/myrmic`, and `rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/myrmic"` reads as `/.local/share/myrmic` when neither `$XDG_DATA_HOME` nor `$HOME` is set.

## Next

[Quickstart](../01_quickstart.md) - build, deploy and call your first cell.
