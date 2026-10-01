# Myrmic e2e tests

---
Prebuild binary of `myrmic` is required. 
---

Writing a myrmic e2e you will need to use the `test_framework::myrmic::Myrmic` type. It knows
the CLI's commands and runs them on one of three backends:
- local binary - `Myrmic::local()`
- binary on a remote host over SSH - `Myrmic::ssh("user@host")`
- binary inside a docker container - `Myrmic::attach(&container)`, borrowing the container, so it
  outlives every runtime and cell guard

Once initialized the `Myrmic` gives a common interface, no matter where the CLI runs. Every
operation returns a `Result`; write `.unwrap()`/`.expect(..)` where the test assumes success.

In any case the first thing you will be doing is starting a runtime:
```rust
let myrmic = Myrmic::local();
// a random name and an in-memory database unless `.name(..)`/`.persistent(true)` say otherwise
let runtime = myrmic.runtime(&["my-tag"]).start().await.unwrap();
// ...
runtime.delete().await.unwrap();
```

The next thing you will usually do is deploying a cell or application:
```rust
// create a new cell and deploy it; the SRN is random unless `.srn(..)` sets one
let cell_spec = myrmic.new_cell("my-cell", None).await.unwrap();
let cell = myrmic.cell(cell_spec).tags(&["my-tag"]).deploy().await.unwrap();
//...
cell.delete().await.unwrap();

// deploy an application specification
let app = myrmic.deploy_app("assets/apps/app_spec.yml").await.unwrap();
```

Both guards keep the deploy's CLI output (`output()`) for tests that assert on it.

After cells or an application have been deployed, you can interact with those. For single cells
the returned `DeployedCell` offers a `send` function that automatically uses the correct SRI. For
applications with multiple cells the `Myrmic` type also offers a `send` function that needs to know
the SRI you are sending to.

With the local backend, `myrmic.connect_session()` opens a zenoh session into the same mesh, e.g.
for a `SorgHandle`.


---
**NOTE**

`Runtime`, `DeployedCell` and `DeployedApp` delete what they stand for when dropped, also when a
test panics. A drop after an explicit `delete()` finds nothing left and stays silent.
---


# Network Tests

---
Prebuild binaries of `swarm`, `test-sidecar` are required. 
---

Network tests always run inside containers and use network shaping to simulate package loss for 
example. The basic structure of such tests can be described as docker compose files and usually
have some preconditions:

```rust
// initialize docker client
let docker = init_docker();

// build the sidecar image
Sidecar::build(
    &docker,
    "assets/dockerfiles/sidecar.dockerfile",
    "../../target/release/test-sidecar",
    "sidecar:network_tests",
)
.await;

// build the swarm image
let test_router_jsonnet = std::path::PathBuf::from("assets/swarm_configs/test_router.jsonnet");
let test_peer_jsonnet = std::path::PathBuf::from("assets/swarm_configs/test_peer.jsonnet");
SwarmImage::build(
    &docker,
    "assets/dockerfiles/swarm.dockerfile",
    "../../source/target/release/swarm",
    "swarm:network_tests",
    &[
        (test_router_jsonnet.as_path(), "test_router.jsonnet"),
        (test_peer_jsonnet.as_path(), "test_peer.jsonnet"),
    ],
)
.await;
```

The example mentions a sidecar image. The sidecar solution solves connection problems under rootless
docker installations (and likely also podman for that reason) as it is not possible to directly 
connect to IP addresses of containers from the host system in that environment. For that reason a
sidecar service is deployed to a specifc network. The sidecar for now offers an incomplete HTTP 
interface of functions that need to connect to services running inside the container, to achieve 
that the sidecar usually exposes a port to the host. For example the sidecar can make connection 
with the `sorg-client` crate and query information from inside the network.

After the general setup above a test can be fairly simple:
```rust
// compose up
let compose = ComposeProject::up(
    "my_compose.yaml",
    "my_project",
)
.await;

// test logic that interacts with sidecar or runs commands inside the compose containers

// compose down
compose.down().await;
```


# Swarm Tests 

---
Prebuild binary of `swarm` is required. 
---

Swarm tests are running typically running locally. The normal test setup up looks like this:
```rust
let swarm = Swarm::local();

let process = swarm
    .spawn(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/path/to/swarm/config.jsonnet"
    ))
    .await;
```

As a second step you would usually build one or more cells and register the artifact(s):
```rust
let cell_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/path/to/test/cell");
let cell_artifact = CellArtifact::build(cell_dir.into()).await;
cell_artifact.register(process.session()).await;
```

For interactions you can then get a wrapper around `sorg-client` to load a cell:
```rust
let mut sorg = SorgHandle::connect(process.session().clone()).await;
sorg.load_cell("cell.wasm", "cell.SRI").await;
```

And from now on you can send command or events via the `SorgHandle`.
