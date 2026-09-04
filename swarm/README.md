## Running tests

For this repository, we assume that the tests are run using [nextest](https://nexte.st/) via the command

```
cargo nextest run
```

following the configuration in `./.config/nextext.toml`.

**Note:** Before running the above commands, you need to run `cargo build` at least once to make sure that the plugins are built and present in the `target/debug` directory (many of the tests need to load them from there).

## Serving Wasm files
The file `./configs/serve_wasm.jsonnet` contains an example Swarm configuration which deploys a filestore plugin which can be used to serve Wasm binaries for Wasm-based operators which are run by the `sorg-execution` plugins. To use a setup where Wasm binaries can be served to execution plugins, you would likely use a very similar `Swarm` configuration. Just make sure that the `root_dir` entry in the `.jsonnet` file points to the directory containing the `.wasm` files (specified relatively to the directory where you execute the `Swarm` binary). Then, you can use the filenames of the `.wasm` files as the `binary` entry in the corresponding application manifest. 