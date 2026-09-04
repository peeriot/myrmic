# Wasm operators - implementing Wasm operators and using them within a application
This second tutorial focuses on the implementation and usage of operator tasks whose functionality is defined by Wasm binaries. The tutorial assumes that you are already familiar with the basics of manifest, swarm config, and CLI usage detailed in the minimal example tutorial. In this next step, we will extend the application from the first tutorial by an operator task implementing a processing step.

## Overview
This tutorial details the aspects relevant to the deployment of applications featuring Wasm-based operators. It describes how these operators are specified in the application manifest, how you can implement their functionality and compile the corresponding Wasm binaries, and how you set up the filestore serving the Wasm binaries during application deployment. As in the last tutorial, these steps are described in the context of an example application.

## Application Manifest
The application manifest specifying the application we focus on in this second part of the tutorial is the file `wasm-operators/manifest.yaml`. Compared to the manifest file from the first tutorial, this one differs by (a) containing an operator task whose functionality is specified by the Wasm binary `even_filter.wasm` and (b) linking the tasks differently, so that the input data is provided to the operator, before the operator output is provided to the sink.

## Example infrastructure
The second tutorial uses the same infrastructure for the running application example: the two executables `publisher` and `subscriber` which are defined under `./code` and built as part of the main `swarm` workspace.

## Implementing a Wasm operator
Wasm operators are implemented as Rust library crates which are compiled to Wasm. You can find a crate implementing the `even_filter` functionality in `./code/even-filter`. 

The `Cargo.toml` of any Wasm operators you would be implementing is likely to look similar to the one provided there. You can use any dependency which will compile to Wasm, making it possible to share dependencies between Wasm operators and/or standalone executables.

For the implementation of Wasm operators, it is recommended to use the libraries `myrmic-sdk` (providing convenient methods for accessing the API offered by the host runtime of the Wasm module) and `myrmic-sdk-macros` (providing an ergonomic way to reference the inputs/outputs of the Wasm task and Rust-like error handling). When annotating the `run` function of a Wasm module, you should use the same names for the inputs/outputs as provided for the corresponding task in the application manifest (`input` and `filtered` in our case). With this, the macro will generate enums which you can use to reference the inputs/outputs when doing calls to the host API functions (`send` and `receive` in this example). Note also that you can emit logs which will be forwarded to the host and logged out by the execution runtime running the corresponding Wasm operator.

The lib crate of a Wasm module can be compiled to a Wasm binary using `cargo` with `wasm32-unknown-unknown` as target.

Since the Wasm target is not available with the default cargo distribution, you may need to first add it by running:

```
rustup target add wasm32-unknown-unknown
```

After the target has been added, you can compile Rust lib crates to Wasm binaries. For instance, you can compile the `even-filter` lib by running:

```
cargo build -p even_filter --target wasm32-unknown-unknown
```

from the `swarm` workspace root. This will generate the Wasm binary `even_filter.wasm` and place it under `./target/wasm32-unknown-unknown/debug`.

(you don't necessarily have to run this command, since it will be run as part of the `build.sh` script of this tutorial)

## Setting up the filestore plugin via a swarm config file
After you've implemented the code of your Wasm operator and compiled it to Wasm, you need to (a) specify the binary in the application manifest (see the `binary` entry of the `even filter` task in the file `./wasm-operators/manifest.yaml`) and (b) set up the filestore plugin which will act as the Wasm registry.

The `binary` entry in the manifest defines the location of the Wasm binary of the operator within the filestore. It is formulated relatively to the **root directory** of the filestore plugin, which you define in the configuration of the filestore plugin. Note that for deploying applications which contain Wasm operators, at least one of the nodes in the system must host a filestore plugin containing the Wasm binaries referenced in the manifest of the application.

For running the application in this example, we will, similarly to the previous chapter of the tutorial, be using a setup with one node. In addition to the execution and the orchestration plugin, the node will also host a filestore plugin. The node configuration we will be using is specified by the config file `./wasm-operators/swarm-config.jsonnet`. 

We configure the root directory as `../../target/wasm32-unknown-unknown/debug`, since we (or rather zellij) will be running the swarm script from the directory `[repo-root]/swarm/tutorials/sorg` -- the root directory of the filestore is provided either as an absolute path or as a path specified relatively to the directory where the `swarm` binary is executed. 

As you see, we configure the directory where the Wasm binaries are being placed after compilation as the root directory of the filestore. Alternatively, it would be possible to define relative paths in the application manifest. The CLI also offers a command to inject files from the local machine into the swarm filestore, but that is something intended rather as a convenience feature for the distributed case (see the `files` subcommand of the `sorg-ctl`).

Note: If you attempt to deploy the swarm node before building the Wasm binary, you will see an error message indicating that the filestore plugin could not be started, since the directory configured as its root does not exist.

## Running the example

### Setup and build
As before, the binaries and plugins are built via the provided `build.sh` script:

```
./wasm-operators/build.sh
```

### Starting zellij
As with the previsou step of the tutorial, we use zellij to run the example. Start a zellij session with the layout of the second tutorial by running:

```
RUST_LOG=info zellij -l ./wasm-operators/layout.kdl
```

(we are using `RUST_LOG=info` here to see the logs emitted by the wasm operator)

You will see the same pane layout as in the first example, with the difference that the swarm script also displays logs about loading and starting the filestore plugin.

### Initializing the application
Next, we initialize the application by running 

```
./sorg-ctl dp init manifest.yaml
```

from the top-right zellij pane.

You should see CTL output indicating a successful initialization of the application. Note that Wasm operators load and initialize the Wasm module as part of their initialization, so that the application can be quickly started thereafter (you likely won't notice much of a delay on your laptop, but loading the binaries takes a while over the network).

### Starting the application
Finally, we start the initialized application by running

```
./sorg-ctl dp start "filter even"
```

Having run this, you should see:

1. The CLI output stating that the application was started (pane on the upper right)
2. The output of the subscriber stating that it receives the even counters (pane on the lower right)
3. The log outputs of the wasm operator, displayed on the WARN level (pane on the left)

### Closing the application
As before, you can close the session using `ctr + Q` (you don't have to delete the application; zellij cleans up when closing the session). 
