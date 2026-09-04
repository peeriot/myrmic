# Tutorial Overview / General Setup

## Overview

These tutorials contain examples and instructions how to set up and start basic applications using the self-organization (sorg) features of swarm.

For these tutorials, we assume that you have access to the [swarm repository](https://github.com/peeriot/swarm) and are going through it after having checked it out on your machine.

## Prerequisites

This tutorial assumes that you

- have installed the current Rust version
- you have `zellij` installed on your system

Note: For the tutorials, zenoh nodes deployed on your system need to be able to use the address `224.0.0.224` for multicasts. This should just work on Linux systems, but we've seen problems with certain VMs.

### Installing Rust
In case you don't have Rust on your system, install it by following the instructions on the [Install Rust](https://www.rust-lang.org/tools/install) website.

Otherwise, update rust by running `rustup update`.

### Zellij
The runnable examples in this tutorial use `zellij` to set up multi-terminal code execution. You can install it by running

```
cargo install zellij
```

Note that the instructions for all tutorials are provided with the assumption that you are in the directory `[repo-root]/swarm/tutorials/sorg` on your machine, where `[repo-root]` is the directory to which you have checked out the [swarm repository](https://github.com/peeriot/swarm).

The example crates under `tutorials/sorg/code` are members of the main `swarm` Cargo workspace. This means that the existing workspace build also covers the tutorial binaries and that all build artifacts end up under `[repo-root]/swarm/target`.

## Available Tutorials

The table below provides an overview of the available tutorials, their `README.md` files, and the concepts that they focus on. It probably makes sense to do them in the order from top to bottom, since the "lower" tutorials assume that the terminology and usage detailed by the "upper" tutorials are understood by the reader.

| Name | Content |
|:--|:--|
|[Minimal Example](../../tutorials/sorg/forward-data/index.md)|Basics of manifest, swarm config, and CLI usage|
|[Wasm Operators](../../tutorials/sorg/wasm-operators/index.md)|Implementing Wasm operators, wasm registry config|
|[Task Mapping](../../tutorials/sorg/task-mapping/index.md)|Types of mapping; node- and task tags|
